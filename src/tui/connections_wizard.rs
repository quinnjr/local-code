//! Pure state machine for the `/connections add` in-TUI stepper, mirroring
//! `mcp_wizard.rs`: side-effect-free and unit-testable separately from
//! `app.rs`'s wiring of it. The wizard collects name → provider → base URL →
//! default model → API key; the caller performs all side effects (keyring
//! write, connections.toml save, OpenRouter catalog fetch) on `Finalize`.

use crate::config::connection::{Connection, ProviderKind};

/// Which question the wizard is currently waiting for an answer to. The
/// provider/base-URL/model collected so far ride along in the step variants
/// so `advance` stays a pure function of (state, line).
#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionsAddStep {
    AskName,
    AskProviderChoice,
    AskBaseUrl {
        provider: ProviderKind,
    },
    AskDefaultModel {
        provider: ProviderKind,
        base_url: String,
    },
    AskApiKey {
        provider: ProviderKind,
        base_url: String,
        default_model: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionsAddWizard {
    pub name: String,
    pub step: ConnectionsAddStep,
}

/// An API key captured by the wizard that still needs to be written to the
/// OS keyring by the caller under the connection's name (the wizard itself
/// is side-effect-free). `None` when the user left the key blank — for
/// OpenRouter that means the `OPENROUTER_API_KEY` env fallback, for local
/// providers it means an unkeyed server.
#[derive(PartialEq)]
pub struct PendingApiKey {
    pub value: String,
}

impl std::fmt::Debug for PendingApiKey {
    /// Redacts `value` so the raw key never lands in test panic messages,
    /// logs, or anything else that formats this with `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingApiKey")
            .field("value", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub struct WizardOutput {
    pub connection: Connection,
    /// `Some` only when the user entered an API key; the caller must store
    /// it (keyed by `connection.name`) *before* saving the connection.
    pub pending_api_key: Option<PendingApiKey>,
}

/// The result of feeding one line of input into `advance`.
#[derive(Debug, PartialEq)]
pub enum Advance {
    /// Move to the next step; the `String` is the prompt to show for it.
    Continue(ConnectionsAddWizard, String),
    /// All fields are collected — build the connection.
    Finalize(WizardOutput),
    /// The line was invalid for the current step (e.g. blank where a value
    /// is required); re-prompt with the same wizard state (the `String` is
    /// the message to show, ending with a repeat of the original prompt).
    Invalid(ConnectionsAddWizard, String),
}

const NAME_PROMPT: &str = "Connection name: ";
const PROVIDER_CHOICE_PROMPT: &str = "Choose a provider (press the digit key):\n\
     1) openai-compatible (llama.cpp / vLLM / LM Studio)\n2) ollama\n3) openrouter";
const BASE_URL_PROMPT: &str = "Base URL: ";
const DEFAULT_MODEL_PROMPT: &str = "Default model: ";
const API_KEY_PROMPT: &str = "API key (blank for none): ";
const OPENROUTER_API_KEY_PROMPT: &str = "API key (blank = OPENROUTER_API_KEY env var): ";

fn base_url_prompt(provider: ProviderKind) -> String {
    if provider == ProviderKind::OpenRouter {
        format!(
            "Base URL [blank = {}]: ",
            crate::config::connection::OPENROUTER_DEFAULT_BASE_URL
        )
    } else {
        BASE_URL_PROMPT.to_string()
    }
}

fn default_model_prompt(provider: ProviderKind) -> String {
    if provider == ProviderKind::OpenRouter {
        // OpenRouter model slugs are namespaced; a bare "gpt-4o" won't
        // resolve, so show the expected shape.
        "Default model (e.g. anthropic/claude-sonnet-4): ".to_string()
    } else {
        DEFAULT_MODEL_PROMPT.to_string()
    }
}

/// Starts a fresh wizard, returning its initial state and the first prompt
/// to show the user.
pub fn start() -> (ConnectionsAddWizard, String) {
    (
        ConnectionsAddWizard {
            name: String::new(),
            step: ConnectionsAddStep::AskName,
        },
        NAME_PROMPT.to_string(),
    )
}

/// Feeds one line of user input (already trimmed of the trailing newline —
/// callers pass the raw input-buffer text) into the wizard's current step.
pub fn advance(wizard: ConnectionsAddWizard, line: &str) -> Advance {
    let trimmed = line.trim();
    match wizard.step.clone() {
        ConnectionsAddStep::AskName => {
            if trimmed.is_empty() {
                return Advance::Invalid(
                    wizard,
                    format!("Connection name cannot be empty. {NAME_PROMPT}"),
                );
            }
            Advance::Continue(
                ConnectionsAddWizard {
                    name: trimmed.to_string(),
                    step: ConnectionsAddStep::AskProviderChoice,
                },
                PROVIDER_CHOICE_PROMPT.to_string(),
            )
        }
        ConnectionsAddStep::AskProviderChoice => {
            let provider = match trimmed {
                "1" => ProviderKind::OpenAiCompatible,
                "2" => ProviderKind::Ollama,
                "3" => ProviderKind::OpenRouter,
                _ => {
                    let msg = format!("'{trimmed}' isn't 1-3.\n{PROVIDER_CHOICE_PROMPT}");
                    return Advance::Invalid(wizard, msg);
                }
            };
            let prompt = base_url_prompt(provider);
            Advance::Continue(
                ConnectionsAddWizard {
                    name: wizard.name,
                    step: ConnectionsAddStep::AskBaseUrl { provider },
                },
                prompt,
            )
        }
        ConnectionsAddStep::AskBaseUrl { provider } => {
            let base_url = if trimmed.is_empty() {
                if provider == ProviderKind::OpenRouter {
                    // OpenRouter has a single canonical endpoint; blank
                    // accepts it (this is the common case).
                    crate::config::connection::OPENROUTER_DEFAULT_BASE_URL.to_string()
                } else {
                    return Advance::Invalid(
                        wizard,
                        format!("Base URL cannot be empty. {BASE_URL_PROMPT}"),
                    );
                }
            } else {
                trimmed.to_string()
            };
            let prompt = default_model_prompt(provider);
            Advance::Continue(
                ConnectionsAddWizard {
                    name: wizard.name,
                    step: ConnectionsAddStep::AskDefaultModel { provider, base_url },
                },
                prompt,
            )
        }
        ConnectionsAddStep::AskDefaultModel { provider, base_url } => {
            if trimmed.is_empty() {
                return Advance::Invalid(
                    wizard,
                    format!(
                        "Default model cannot be empty. {}",
                        default_model_prompt(provider)
                    ),
                );
            }
            let key_prompt = if provider == ProviderKind::OpenRouter {
                OPENROUTER_API_KEY_PROMPT
            } else {
                API_KEY_PROMPT
            };
            Advance::Continue(
                ConnectionsAddWizard {
                    name: wizard.name,
                    step: ConnectionsAddStep::AskApiKey {
                        provider,
                        base_url,
                        default_model: trimmed.to_string(),
                    },
                },
                key_prompt.to_string(),
            )
        }
        ConnectionsAddStep::AskApiKey {
            provider,
            base_url,
            default_model,
        } => {
            let pending_api_key = if trimmed.is_empty() {
                None
            } else {
                Some(PendingApiKey {
                    value: trimmed.to_string(),
                })
            };
            Advance::Finalize(WizardOutput {
                connection: Connection {
                    name: wizard.name,
                    provider,
                    base_url,
                    default_model,
                    // OpenRouter's catalog is fetched by the caller between
                    // Finalize and save; local providers keep this empty
                    // until the user fills `models` in connections.toml.
                    models: vec![],
                },
                pending_api_key,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_api_key_debug_redacts_value() {
        let pending = PendingApiKey {
            value: "sk-or-secret".to_string(),
        };
        let debug = format!("{pending:?}");
        assert!(!debug.contains("sk-or-secret"));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn empty_name_is_invalid_and_keeps_asking_for_a_name() {
        let (wizard, _) = start();
        match advance(wizard, "") {
            Advance::Invalid(w, msg) => {
                assert_eq!(w.step, ConnectionsAddStep::AskName);
                assert!(msg.contains("cannot be empty"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn bad_provider_digit_is_invalid() {
        let (wizard, _) = start();
        let Advance::Continue(wizard, _) = advance(wizard, "my-conn") else {
            panic!("expected Continue")
        };
        match advance(wizard, "4") {
            Advance::Invalid(w, msg) => {
                assert_eq!(w.step, ConnectionsAddStep::AskProviderChoice);
                assert!(msg.contains("isn't 1-3"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    fn past_provider_choice(digit: &str) -> ConnectionsAddWizard {
        let (wizard, _) = start();
        let Advance::Continue(wizard, _) = advance(wizard, "my-conn") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, digit) else {
            panic!("expected Continue")
        };
        wizard
    }

    #[test]
    fn blank_base_url_is_invalid_for_local_providers() {
        for digit in ["1", "2"] {
            let wizard = past_provider_choice(digit);
            match advance(wizard, "") {
                Advance::Invalid(_, msg) => assert!(msg.contains("cannot be empty")),
                other => panic!("expected Invalid for digit {digit}, got {other:?}"),
            }
        }
    }

    #[test]
    fn blank_base_url_defaults_to_the_openrouter_endpoint() {
        let wizard = past_provider_choice("3");
        let Advance::Continue(wizard, prompt) = advance(wizard, "") else {
            panic!("expected Continue")
        };
        let ConnectionsAddStep::AskDefaultModel { provider, base_url } = &wizard.step else {
            panic!("expected AskDefaultModel, got {wizard:?}")
        };
        assert_eq!(*provider, ProviderKind::OpenRouter);
        assert_eq!(
            base_url,
            crate::config::connection::OPENROUTER_DEFAULT_BASE_URL
        );
        assert!(prompt.contains("e.g."), "openrouter model hint: {prompt}");
    }

    #[test]
    fn blank_default_model_is_invalid() {
        let wizard = past_provider_choice("3");
        let Advance::Continue(wizard, _) = advance(wizard, "") else {
            panic!("expected Continue")
        };
        match advance(wizard, "") {
            Advance::Invalid(_, msg) => assert!(msg.contains("cannot be empty")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn openrouter_branch_finalizes_with_key_and_empty_models() {
        let wizard = past_provider_choice("3");
        let Advance::Continue(wizard, _) = advance(wizard, "") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, prompt) = advance(wizard, "anthropic/claude-sonnet-4") else {
            panic!("expected Continue")
        };
        assert!(prompt.contains("OPENROUTER_API_KEY"), "{prompt}");
        match advance(wizard, "sk-or-test") {
            Advance::Finalize(output) => {
                assert_eq!(
                    output.connection,
                    Connection {
                        name: "my-conn".into(),
                        provider: ProviderKind::OpenRouter,
                        base_url: crate::config::connection::OPENROUTER_DEFAULT_BASE_URL.into(),
                        default_model: "anthropic/claude-sonnet-4".into(),
                        models: vec![],
                    }
                );
                assert_eq!(
                    output.pending_api_key,
                    Some(PendingApiKey {
                        value: "sk-or-test".into()
                    })
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn openrouter_branch_with_blank_key_finalizes_with_no_pending_key() {
        let wizard = past_provider_choice("3");
        let Advance::Continue(wizard, _) = advance(wizard, "") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, "openai/gpt-4o") else {
            panic!("expected Continue")
        };
        match advance(wizard, "") {
            Advance::Finalize(output) => {
                assert_eq!(output.pending_api_key, None);
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn ollama_branch_finalizes_with_entered_url_and_no_key() {
        let wizard = past_provider_choice("2");
        let Advance::Continue(wizard, prompt) = advance(wizard, "http://localhost:11434") else {
            panic!("expected Continue")
        };
        assert_eq!(prompt, DEFAULT_MODEL_PROMPT);
        let Advance::Continue(wizard, prompt) = advance(wizard, "llama3.1") else {
            panic!("expected Continue")
        };
        assert_eq!(prompt, API_KEY_PROMPT);
        match advance(wizard, "") {
            Advance::Finalize(output) => {
                assert_eq!(
                    output.connection,
                    Connection {
                        name: "my-conn".into(),
                        provider: ProviderKind::Ollama,
                        base_url: "http://localhost:11434".into(),
                        default_model: "llama3.1".into(),
                        models: vec![],
                    }
                );
                assert_eq!(output.pending_api_key, None);
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn openai_compatible_branch_finalizes_with_key() {
        let wizard = past_provider_choice("1");
        let Advance::Continue(wizard, _) = advance(wizard, "http://localhost:8000/v1") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, "qwen2.5-coder-32b") else {
            panic!("expected Continue")
        };
        match advance(wizard, "sk-local") {
            Advance::Finalize(output) => {
                assert_eq!(output.connection.provider, ProviderKind::OpenAiCompatible);
                assert_eq!(
                    output.pending_api_key,
                    Some(PendingApiKey {
                        value: "sk-local".into()
                    })
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }
}
