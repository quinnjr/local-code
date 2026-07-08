//! Integration test against a real Ollama server. Requires:
//!   - Ollama running locally (default `http://localhost:11434`) with a model
//!     pulled that supports tool calling (e.g. `llama3.1`).
//!   - `LOCAL_CODE_TEST_OLLAMA_MODEL` set to that model's name.
//!
//! Run with: `cargo test --test live_ollama -- --ignored --nocapture`

use std::sync::Arc;

use local_code::agent::build::build_agent;
use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};
use local_code::permissions::gate::PermissionGate;
use local_code::permissions::settings::PermissionSettings;
use local_code::permissions::stdio::StdioPrompter;
use local_code::permissions::types::PermissionTier;

#[tokio::test]
#[ignore = "requires a real local Ollama server with a tool-calling-capable model pulled"]
async fn prompts_a_real_ollama_server_and_gets_a_text_response() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OLLAMA_MODEL")
        .unwrap_or_else(|_| "llama3.1".to_string());

    let connection = Connection {
        name: "live-ollama-test".into(),
        provider: ProviderKind::Ollama,
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
