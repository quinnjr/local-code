//! Integration test proving `/init`'s generation call produces nonempty
//! AGENTS.md content against a real local server. Run with:
//! `cargo test --test live_init -- --ignored --nocapture`

use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};
use local_code::init::{generate_agents_md, survey_project};

#[tokio::test]
#[ignore = "requires a real local OpenAI-compatible server"]
async fn generates_nonempty_agents_md_for_this_repo() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OPENAI_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OPENAI_MODEL")
        .expect("set LOCAL_CODE_TEST_OPENAI_MODEL to a model your server has loaded");

    let connection = Connection {
        name: "live-init-test".into(),
        provider: ProviderKind::OpenAiCompatible,
        base_url,
        default_model: model_id,
        models: vec![],
    };
    let model = build_model(&connection, None).expect("model construction should not fail");

    let survey = survey_project(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let content = generate_agents_md(&model, &survey).await.expect("generation should succeed");
    assert!(!content.trim().is_empty());
}
