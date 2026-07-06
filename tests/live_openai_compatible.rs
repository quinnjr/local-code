//! Integration test against a real OpenAI-compatible local server (llama.cpp
//! server, vLLM, LM Studio, text-generation-webui). Requires:
//!   - a server listening at `LOCAL_CODE_TEST_OPENAI_BASE_URL` (default
//!     `http://localhost:8000/v1`) that supports native OpenAI-style `tool_calls`.
//!   - `LOCAL_CODE_TEST_OPENAI_MODEL` set to a model ID the server has loaded.
//! Run with: `cargo test --test live_openai_compatible -- --ignored --nocapture`

use std::sync::Arc;

use local_code::agent::build::build_agent;
use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};
use local_code::permissions::gate::PermissionGate;
use local_code::permissions::settings::PermissionSettings;
use local_code::permissions::stdio::StdioPrompter;
use local_code::permissions::types::PermissionTier;

#[tokio::test]
#[ignore = "requires a real local OpenAI-compatible server with tool_calls support"]
async fn prompts_a_real_openai_compatible_server_and_gets_a_text_response() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OPENAI_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OPENAI_MODEL")
        .expect("set LOCAL_CODE_TEST_OPENAI_MODEL to a model your server has loaded");

    let connection = Connection {
        name: "live-test".into(),
        provider: ProviderKind::OpenAiCompatible,
        base_url,
        default_model: model_id,
        models: vec![],
    };

    let model = build_model(&connection, None).expect("model construction should not fail");
    let gate = Arc::new(PermissionGate::new(
        PermissionTier::FullAuto,
        PermissionSettings::default(),
        Arc::new(StdioPrompter::real()),
    ));
    let agent = build_agent(model, gate).expect("agent construction should not fail");

    let response = agent
        .prompt("Reply with exactly the word: pong")
        .await
        .expect("prompt should succeed against a live server");

    assert!(!response.text().is_empty());
}
