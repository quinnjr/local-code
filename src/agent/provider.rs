use daimon::model::SharedModel;
use daimon::model::local::{Ollama, OpenAiCompatible};
use daimon::model::openrouter::OpenRouter;

use crate::config::connection::{Connection, ProviderKind};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("connection '{0}' has an empty base_url")]
    EmptyBaseUrl(String),
    #[error("failed to fetch the model catalog: {0}")]
    Catalog(String),
}

/// Builds a `daimon` `Model` (erased behind `SharedModel`) from a resolved `Connection`
/// and its (optional) API key. `OpenAiCompatible` connections use
/// `daimon-provider-local`'s generic OpenAI-compatible provider pointed at
/// `connection.base_url`; `Ollama` connections use the dedicated Ollama provider;
/// `OpenRouter` connections use `daimon-provider-openrouter` (the `openrouter`
/// feature). Later phases (`/model` switching) call this directly.
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
            // The OpenAI-standard field; llama.cpp, vLLM and LM Studio all
            // honor it for reasoning models and ignore it otherwise. The
            // Ollama/OpenRouter arms below have no equivalent hook in daimon
            // 0.23 (see `effort::supports_effort`), so the setting is
            // silently unused there — callers surface that themselves.
            if let Some(effort) = connection.effort {
                m = m.with_extra_field("reasoning_effort", serde_json::json!(effort.as_str()));
            }
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
        ProviderKind::OpenRouter => {
            // A stored keyring key wins; without one, `OpenRouter::new`
            // falls back to the OPENROUTER_API_KEY environment variable (the
            // crate warns if that is unset too — OpenRouter requires a key,
            // so requests would then fail with a 401 naming the problem).
            let m = match api_key.filter(|k| !k.is_empty()) {
                Some(key) => OpenRouter::with_api_key(connection.default_model.clone(), key),
                None => OpenRouter::new(connection.default_model.clone()),
            };
            std::sync::Arc::new(
                m.with_base_url(connection.base_url.clone())
                    .with_app_name("local-code")
                    .with_timeout(std::time::Duration::from_secs(300)),
            )
        }
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

/// Fetches the model catalog (`GET {base_url}/models`) of an OpenAI-compatible
/// server — OpenRouter's catalog, or a local server's loaded models —
/// returning the ids the `/model` picker offers. Used by the `/connections
/// add` wizard to populate a new connection's `models` list.
///
/// The catalog is public on OpenRouter, so `api_key` is optional (pass the
/// key when one is known — authenticated calls don't share the anonymous
/// rate limit). Deliberately short-timeout: callers treat an error as "no
/// listing available", never as fatal.
pub async fn list_models(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>, ProviderError> {
    #[derive(serde::Deserialize)]
    struct ModelList {
        data: Vec<ModelEntry>,
    }
    #[derive(serde::Deserialize)]
    struct ModelEntry {
        id: String,
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| ProviderError::Catalog(format!("HTTP client build failed: {e}")))?;
    let url = format!("{}/models", base_url.trim().trim_end_matches('/'));
    let api_key = api_key.filter(|k| !k.is_empty());
    ensure_not_plaintext_remote(&url, api_key.is_some())?;
    let mut request = client.get(&url);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|e| ProviderError::Catalog(format!("HTTP error: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(ProviderError::Catalog(format!(
            "API error ({status}): {}",
            extract_error_message(&text)
        )));
    }
    let list = response
        .json::<ModelList>()
        .await
        .map_err(|e| ProviderError::Catalog(format!("response parse error: {e}")))?;
    Ok(list.data.into_iter().map(|m| m.id).collect())
}

/// Refuses to send an API key over plaintext `http://` to a non-loopback
/// host — the same guard `daimon`'s providers apply to chat requests,
/// applied here to the catalog fetch (loopback is exempt: test servers and
/// keyed local deployments are normally plaintext).
fn ensure_not_plaintext_remote(base_url: &str, has_key: bool) -> Result<(), ProviderError> {
    if !has_key {
        return Ok(());
    }
    let Ok(url) = reqwest::Url::parse(base_url) else {
        // Not parseable as a URL; reqwest's send error will name the problem.
        return Ok(());
    };
    if url.scheme() != "http" {
        return Ok(());
    }
    let host = url.host_str().unwrap_or_default();
    let loopback = host == "localhost"
        || host.ends_with(".localhost")
        || host.starts_with("127.")
        || host == "::1"
        || host == "[::1]";
    if loopback {
        return Ok(());
    }
    Err(ProviderError::Catalog(format!(
        "refusing to send the API key over plaintext http:// to '{host}'; \
         use https:// or remove the key"
    )))
}

/// OpenAI-style servers return errors as `{"error": {"message": "...", ...}}`.
/// Extract the human message so error strings don't dump the raw JSON
/// envelope; fall back to the raw body for non-conforming responses (e.g. an
/// HTML error page from a proxy).
fn extract_error_message(body: &str) -> String {
    #[derive(serde::Deserialize)]
    struct ErrorEnvelope {
        error: ErrorBody,
    }
    #[derive(serde::Deserialize)]
    struct ErrorBody {
        message: String,
    }

    match serde_json::from_str::<ErrorEnvelope>(body) {
        Ok(envelope) => envelope.error.message,
        Err(_) => body.to_string(),
    }
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
            effort: None,
        }
    }

    fn ollama_connection() -> Connection {
        Connection {
            name: "home-ollama".into(),
            provider: ProviderKind::Ollama,
            base_url: "http://localhost:11434".into(),
            default_model: "llama3.1".into(),
            models: vec![],
            effort: None,
        }
    }

    fn openrouter_connection() -> Connection {
        Connection {
            name: "openrouter".into(),
            provider: ProviderKind::OpenRouter,
            base_url: "https://openrouter.ai/api/v1".into(),
            default_model: "anthropic/claude-sonnet-4".into(),
            models: vec![],
            effort: None,
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
    fn builds_openrouter_model_with_key() {
        let result = build_model(&openrouter_connection(), Some("sk-or-test".into()));
        assert!(result.is_ok());
    }

    #[test]
    fn builds_openrouter_model_without_key_falls_back_to_env() {
        // No keyring key: construction still succeeds — the crate reads
        // OPENROUTER_API_KEY (unset in tests, so it just warns) and any
        // failure surfaces later as a 401 from OpenRouter itself.
        let result = build_model(&openrouter_connection(), None);
        assert!(result.is_ok());
    }

    /// Serves one canned chat completion and records whether the request
    /// body carried `reasoning_effort` — the only observable effect of
    /// `Connection.effort`, since `OpenAiCompatible`'s fields are private.
    async fn chat_completion_server(expect_effort: Option<&str>) -> wiremock::MockServer {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let response = ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "model": "qwen3",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
        let mock = Mock::given(method("POST")).and(path("/v1/chat/completions"));
        let mock = match expect_effort {
            Some(effort) => mock.and(body_partial_json(
                serde_json::json!({"reasoning_effort": effort}),
            )),
            None => mock.and(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                body.get("reasoning_effort").is_none()
            }),
        };
        mock.respond_with(response).expect(1).mount(&server).await;
        server
    }

    #[tokio::test]
    async fn openai_compatible_sends_reasoning_effort_when_set() {
        let server = chat_completion_server(Some("high")).await;
        let mut conn = openai_connection();
        conn.base_url = format!("{}/v1", server.uri());
        conn.effort = Some(crate::agent::effort::ReasoningEffort::High);
        let model = build_model(&conn, None).unwrap();
        let request =
            daimon::model::types::ChatRequest::new(vec![daimon::model::types::Message::user("hi")]);
        let response = model.generate_erased(&request).await.unwrap();
        assert_eq!(response.text(), "ok");
        // `.expect(1)` on the mock verifies the matcher (effort present) on drop.
    }

    #[tokio::test]
    async fn openai_compatible_omits_reasoning_effort_when_unset() {
        let server = chat_completion_server(None).await;
        let mut conn = openai_connection();
        conn.base_url = format!("{}/v1", server.uri());
        let model = build_model(&conn, None).unwrap();
        let request =
            daimon::model::types::ChatRequest::new(vec![daimon::model::types::Message::user("hi")]);
        let response = model.generate_erased(&request).await.unwrap();
        assert_eq!(response.text(), "ok");
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

    #[test]
    fn plaintext_guard_allows_https_and_loopback_with_a_key() {
        assert!(ensure_not_plaintext_remote("https://openrouter.ai/api/v1/models", true).is_ok());
        assert!(ensure_not_plaintext_remote("http://localhost:8000/v1/models", true).is_ok());
        assert!(ensure_not_plaintext_remote("http://127.0.0.1:8000/v1/models", true).is_ok());
        // No key: plaintext to a remote host is fine (nothing to leak).
        assert!(ensure_not_plaintext_remote("http://example.com/v1/models", false).is_ok());
    }

    #[test]
    fn plaintext_guard_refuses_keyed_plaintext_to_a_remote_host() {
        let err = ensure_not_plaintext_remote("http://openrouter.example.com/api/v1/models", true)
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Catalog(ref msg) if msg.contains("plaintext")),
            "expected a plaintext-refusal Catalog error, got {err}"
        );
    }

    #[test]
    fn extract_error_message_reads_the_openai_error_envelope() {
        let body = r#"{"error": {"message": "Invalid API key", "code": 401}}"#;
        assert_eq!(extract_error_message(body), "Invalid API key");
        // Non-conforming bodies (proxy HTML pages, plain text) pass through.
        assert_eq!(extract_error_message("gateway timeout"), "gateway timeout");
    }
}
