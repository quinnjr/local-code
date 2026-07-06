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
                    .with_base_url(connection.base_url.clone()),
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
