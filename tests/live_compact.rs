//! Integration test proving `/compact`'s summarization logic works against a
//! real local server's non-streaming `generate` endpoint. Requires the same
//! environment variables as `tests/live_openai_compatible.rs` from Phase 2.
//! Run with: `cargo test --test live_compact -- --ignored --nocapture`

use daimon::model::types::{ChatRequest, Message};
use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};

#[tokio::test]
#[ignore = "requires a real local OpenAI-compatible server"]
async fn summarization_call_returns_nonempty_text() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OPENAI_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OPENAI_MODEL")
        .expect("set LOCAL_CODE_TEST_OPENAI_MODEL to a model your server has loaded");

    let connection = Connection {
        name: "live-compact-test".into(),
        provider: ProviderKind::OpenAiCompatible,
        base_url,
        default_model: model_id,
        models: vec![],
    };
    let model = build_model(&connection, None).expect("model construction should not fail");

    let request = ChatRequest {
        messages: vec![
            Message::system("Summarize the following conversation in one sentence."),
            Message::user("User: what's 2+2?\nAssistant: 4."),
        ],
        tools: Vec::new(),
        temperature: Some(0.0),
        max_tokens: Some(128),
    };

    let response = model.generate_erased(&request).await.expect("summarization call should succeed");
    assert!(!response.text().is_empty());
}
