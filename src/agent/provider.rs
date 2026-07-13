// src/agent/provider.rs

use daimon::model::SharedModel;
use daimon::model::local::{Ollama, OpenAiCompatible};

use crate::config::connection::{Connection, ProviderKind};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("connection '{0}' has an empty base_url")]
    EmptyBaseUrl(String),
}

/// Builds a `daimon` `Model` (erased behind `SharedModel`) from a resolved `Connection`
/// and its (optional) API key. `OpenAiCompatible` connections use
/// `daimon-provider-local`'s generic OpenAI-compatible provider pointed at
/// `connection.base_url`; `Ollama` connections use the dedicated Ollama provider.
/// Later phases (`/model` switching) call this directly.
pub fn build_model(
    connection: &Connection,
    api_key: Option<String>,
) -> Result<SharedModel, ProviderError> {
    if connection.base_url.trim().is_empty() {
        return Err(ProviderError::EmptyBaseUrl(connection.name.clone()));
    }

    let model: SharedModel = match connection.provider {
        ProviderKind::OpenAiCompatible => {
            let mut m =
                OpenAiCompatible::new(normalize_openai_compatible_base_url(&connection.base_url))
                    .with_model(connection.default_model.clone())
                    .with_timeout(std::time::Duration::from_secs(300));
            if let Some(key) = api_key.filter(|k| !k.is_empty()) {
                // Since daimon 0.22, sending an API key over plaintext `http://` is a
                // hard error unless explicitly allowed. This binary only ever talks to
                // local/local-network servers (vLLM `--api-key`, LM Studio, ...), where
                // keyed-but-plaintext is the normal deployment, so opt in here rather
                // than break every keyed local connection.
                m = m.with_api_key(key).allow_plaintext_api_key();
            }
            std::sync::Arc::new(m)
        }
        ProviderKind::Ollama => std::sync::Arc::new(
            Ollama::new(connection.default_model.clone())
                .with_base_url(connection.base_url.clone()),
        ),
    };

    Ok(model)
}

/// Connections have always stored OpenAI-compatible base URLs with the `/v1`
/// API prefix included (e.g. `http://localhost:8000/v1`), because the old
/// provider appended only `/chat/completions`. `daimon-provider-local`'s
/// `OpenAiCompatible` instead appends the full `/v1/chat/completions` path,
/// so a stored `/v1` suffix must be stripped or requests would hit
/// `/v1/v1/chat/completions`.
fn normalize_openai_compatible_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
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
    // `OpenAiCompatible` struct stores its HTTP settings in private fields with no
    // public getters, so even before erasure there is no way to read them back from
    // outside the `daimon-provider-local` crate. If the `.with_timeout(...)` call in
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

    #[test]
    fn strips_v1_suffix_from_stored_base_urls() {
        assert_eq!(
            normalize_openai_compatible_base_url("http://localhost:8000/v1"),
            "http://localhost:8000"
        );
        assert_eq!(
            normalize_openai_compatible_base_url("http://localhost:8000/v1/"),
            "http://localhost:8000"
        );
    }

    #[test]
    fn leaves_bare_base_urls_alone() {
        assert_eq!(
            normalize_openai_compatible_base_url("http://localhost:8000"),
            "http://localhost:8000"
        );
        assert_eq!(
            normalize_openai_compatible_base_url("http://localhost:8000/"),
            "http://localhost:8000"
        );
    }

    #[test]
    fn preserves_non_v1_path_prefixes() {
        assert_eq!(
            normalize_openai_compatible_base_url("http://host:8080/serve/v1"),
            "http://host:8080/serve"
        );
        assert_eq!(
            normalize_openai_compatible_base_url("http://host:8080/serve"),
            "http://host:8080/serve"
        );
    }
}
