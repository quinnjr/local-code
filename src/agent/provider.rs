// src/agent/provider.rs

use daimon::model::SharedModel;
use daimon::model::ollama::Ollama;
use daimon::model::openai::OpenAi;

use crate::config::connection::{Connection, ProviderKind};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("connection '{0}' has an empty base_url")]
    EmptyBaseUrl(String),
}

/// Builds a `daimon` `Model` (erased behind `SharedModel`) from a resolved `Connection`
/// and its (optional) API key. `OpenAiCompatible` connections use `daimon`'s generic
/// OpenAI-compatible provider pointed at `connection.base_url`; `Ollama` connections use
/// the dedicated Ollama provider. Later phases (`/model` switching) call this directly.
pub fn build_model(connection: &Connection, api_key: Option<String>) -> Result<SharedModel, ProviderError> {
    if connection.base_url.trim().is_empty() {
        return Err(ProviderError::EmptyBaseUrl(connection.name.clone()));
    }

    let model: SharedModel = match connection.provider {
        ProviderKind::OpenAiCompatible => {
            let key = api_key.unwrap_or_default();
            std::sync::Arc::new(
                OpenAi::with_api_key(connection.default_model.clone(), key)
                    .with_base_url(connection.base_url.clone())
                    .with_timeout(std::time::Duration::from_secs(300)),
            )
        }
        ProviderKind::Ollama => std::sync::Arc::new(
            Ollama::new(connection.default_model.clone()).with_base_url(connection.base_url.clone()),
        ),
    };

    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai_connection() -> Connection {
        Connection {
            name: "local-vllm".into(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:8000/v1".into(),
            default_model: "qwen2.5-coder-32b".into(),
            models: vec![],
        }
    }

    fn ollama_connection() -> Connection {
        Connection {
            name: "home-ollama".into(),
            provider: ProviderKind::Ollama,
            base_url: "http://localhost:11434".into(),
            default_model: "llama3.1".into(),
            models: vec![],
        }
    }

    // NOTE: these tests cannot assert that `.with_timeout(..)` actually took effect.
    // `build_model` returns `SharedModel = Arc<dyn ErasedModel>` (see
    // `daimon_core::model::ErasedModel`), which has no `Any`/downcast supertrait, no
    // `Debug` bound, and no accessor for the configured timeout. `daimon`'s concrete
    // `OpenAi` struct (daimon-0.16.0/src/model/openai.rs) stores `timeout` as a private
    // field with no public getter, so even before erasure there is no way to read it
    // back from outside the `daimon` crate. If the `.with_timeout(...)` call in
    // `build_model` is ever dropped or reordered, these tests will keep passing with
    // no signal of the regression — there is currently no introspection path available
    // to close that gap.
    #[test]
    fn builds_openai_compatible_model_without_key() {
        let result = build_model(&openai_connection(), None);
        assert!(result.is_ok());
    }

    #[test]
    fn builds_openai_compatible_model_with_key() {
        let result = build_model(&openai_connection(), Some("sk-test".into()));
        assert!(result.is_ok());
    }

    #[test]
    fn builds_ollama_model() {
        let result = build_model(&ollama_connection(), None);
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_empty_base_url() {
        let mut conn = openai_connection();
        conn.base_url = "  ".into();
        let result = build_model(&conn, None);
        assert!(matches!(result, Err(ProviderError::EmptyBaseUrl(name)) if name == "local-vllm"));
    }
}
