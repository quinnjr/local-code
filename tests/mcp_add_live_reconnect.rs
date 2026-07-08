//! Exercises `/mcp add`'s live-reconnect *success* path end to end — the
//! feature's core value proposition (per `TODO.md`'s note that `/mcp add`
//! is the one path that connects and merges tools into the live agent
//! immediately). Every wizard test in `src/tui/app.rs`'s own test module
//! deliberately targets an unreachable server, so only the `Err` branch of
//! the finalize handler gets covered there — this test drives the `Ok`
//! branch (tool merge + agent rebuild) against the real fixture MCP server
//! (see `tests/mcp_stdio_integration.rs`), which needs `CARGO_BIN_EXE_*`,
//! only available to tests under `tests/`, not `src/tui/app.rs`'s `--lib`
//! unit tests.

use std::sync::Arc;

use daimon::model::types::{ChatRequest, ChatResponse, StopReason, Usage};
use daimon::stream::{ResponseStream, StreamEvent};
use ntui::testing::TestTerminal;
use ntui::{Element, KeyCode};

use local_code::permissions::types::PermissionTier;
use local_code::tui::app::{App, AppProps};

struct StreamingEchoModel;
impl daimon::model::Model for StreamingEchoModel {
    async fn generate(&self, _request: &ChatRequest) -> daimon::Result<ChatResponse> {
        Ok(ChatResponse {
            message: daimon::model::types::Message::assistant("unused"),
            stop_reason: StopReason::EndTurn,
            usage: Some(Usage::default()),
        })
    }
    async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta("hi".into())),
            Ok(StreamEvent::Done),
        ])))
    }
}

fn test_props(dir: &std::path::Path) -> AppProps {
    AppProps {
        model: Some(Arc::new(StreamingEchoModel)),
        connection_name: "local-vllm".into(),
        model_name: "qwen2.5-coder-32b".into(),
        always_allow: vec![],
        always_deny: vec![],
        initial_tier: PermissionTier::FullAuto,
        project_config_dir: dir.to_path_buf(),
        user_config_dir: dir.join("user-config-unused"),
        ..AppProps::default()
    }
}

async fn type_and_submit(t: &mut TestTerminal, text: &str) {
    for c in text.chars() {
        t.send_key(KeyCode::Char(c)).unwrap();
    }
    t.send_key(KeyCode::Enter).unwrap();
}

#[tokio::test]
async fn mcp_add_wizard_connects_successfully_and_merges_new_tools_live() {
    let dir = tempfile::tempdir().unwrap();
    let props = test_props(dir.path());
    let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();

    type_and_submit(&mut t, "/mcp add").await;
    t.tick().await.unwrap();
    type_and_submit(&mut t, "fixture").await;
    t.tick().await.unwrap();
    type_and_submit(&mut t, "3").await; // custom stdio command
    t.tick().await.unwrap();
    assert!(t.frame_text().contains("Command:"), "{}", t.frame_text());

    type_and_submit(&mut t, env!("CARGO_BIN_EXE_local-code")).await;
    t.tick().await.unwrap();
    type_and_submit(&mut t, "__mcp_fixture_server").await;
    t.tick().await.unwrap();
    type_and_submit(&mut t, "").await; // finish the args loop

    let mut text = String::new();
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        t.tick().await.unwrap();
        text = t.frame_text();
        if text.contains("connected") || text.contains("failed") {
            break;
        }
    }
    assert!(text.contains("connected — 1 tools added"), "{text}");

    let saved = local_code::config::mcp_servers::load_mcp_servers(
        std::path::Path::new("/nonexistent"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "fixture");

    // Confirm the tool actually took effect on the live (rebuilt) agent, not
    // just that a "connected" notice was printed: submit a turn and let the
    // (echoing) model run — this doesn't call the tool, but proves the
    // wizard's rebuild didn't leave the TUI stuck or the agent broken.
    type_and_submit(&mut t, "hello").await;
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        t.tick().await.unwrap();
    }
    assert!(t.frame_text().contains("hi"), "{}", t.frame_text());
}

#[tokio::test]
async fn mcp_add_wizard_reconnecting_the_same_server_name_does_not_duplicate_tools() {
    let dir = tempfile::tempdir().unwrap();
    let props = test_props(dir.path());
    let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();

    for _ in 0..2 {
        type_and_submit(&mut t, "/mcp add").await;
        t.tick().await.unwrap();
        type_and_submit(&mut t, "fixture").await;
        t.tick().await.unwrap();
        type_and_submit(&mut t, "3").await;
        t.tick().await.unwrap();
        type_and_submit(&mut t, env!("CARGO_BIN_EXE_local-code")).await;
        t.tick().await.unwrap();
        type_and_submit(&mut t, "__mcp_fixture_server").await;
        t.tick().await.unwrap();
        type_and_submit(&mut t, "").await;

        let mut text = String::new();
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            t.tick().await.unwrap();
            text = t.frame_text();
            if text.contains("connected") || text.contains("failed") {
                break;
            }
        }
        assert!(text.contains("connected — 1 tools added"), "{text}");
    }

    // Only one server on disk (save already retains-by-name), and the second
    // connect's "1 tools added" (not "2") confirms the live tool list didn't
    // silently accumulate a stale duplicate under the same `fixture__echo` key.
    let saved = local_code::config::mcp_servers::load_mcp_servers(
        std::path::Path::new("/nonexistent"),
        dir.path(),
    )
    .unwrap();
    assert_eq!(saved.len(), 1);
}
