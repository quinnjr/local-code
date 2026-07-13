// src/tui/mcp_wizard.rs

use std::collections::HashMap;

use crate::config::mcp_servers::{McpServerConfig, McpTransportConfig};
use crate::config::secrets::sanitize_secret_name;

/// Which question the wizard is currently waiting for an answer to. Args are
/// collected one line at a time (blank line finishes the loop) rather than
/// space-separated on one line, so an argument containing a space (e.g. a
/// file path) doesn't need any quoting convention.
#[derive(Clone, Debug, PartialEq)]
pub enum McpAddStep {
    AskName,
    AskTransportChoice,
    AskNpmPackage,
    AskNpmArgs { package: String, args: Vec<String> },
    AskPipxPackage,
    AskPipxArgs { package: String, args: Vec<String> },
    AskCustomCommand,
    AskCustomArgs { command: String, args: Vec<String> },
    AskHttpUrl,
    AskHttpBearer { url: String },
    AskSseUrl,
    AskSseBearer { url: String },
    AskWebsocketUrl,
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpAddWizard {
    pub name: String,
    pub step: McpAddStep,
}

/// A bearer token captured by the wizard that still needs to be written to
/// the OS keyring by the caller (the wizard itself is side-effect-free).
#[derive(PartialEq)]
pub struct PendingSecret {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for PendingSecret {
    /// Redacts `value` so the raw token never lands in test panic messages,
    /// logs, or anything else that formats this with `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSecret")
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub struct WizardOutput {
    pub config: McpServerConfig,
    /// `Some` only when the user entered a bearer token; the caller must
    /// store it under this name *before* saving the config, which references
    /// it as `${keyring:<name>}`.
    pub pending_secret: Option<PendingSecret>,
}

/// The result of feeding one line of input into `advance`.
#[derive(Debug, PartialEq)]
pub enum Advance {
    /// Move to the next step; the `String` is the prompt to show for it.
    Continue(McpAddWizard, String),
    /// All required fields are collected — build the server config.
    Finalize(WizardOutput),
    /// The line was invalid for the current step (e.g. blank where a value
    /// is required); re-prompt with the same wizard state (the `String` is
    /// the message to show, ending with a repeat of the original prompt).
    Invalid(McpAddWizard, String),
}

const TRANSPORT_CHOICE_PROMPT: &str = "Choose a transport (press the digit key):\n\
     1) npm package\n2) pipx package\n3) custom stdio command\n4) HTTP URL\n5) SSE URL\n6) WebSocket URL";
const ARGS_PROMPT: &str = "Extra args, one per line (blank line to finish): ";
const NAME_PROMPT: &str = "Server name: ";
const NPM_PACKAGE_PROMPT: &str = "npm package name: ";
const PIPX_PACKAGE_PROMPT: &str = "pipx package name: ";
const COMMAND_PROMPT: &str = "Command: ";
const HTTP_URL_PROMPT: &str = "HTTP URL: ";
const SSE_URL_PROMPT: &str = "SSE URL: ";
const WEBSOCKET_URL_PROMPT: &str = "WebSocket URL: ";
const BEARER_PROMPT: &str = "Bearer token (blank for none): ";

/// Starts a fresh wizard, returning its initial state and the first prompt
/// to show the user.
pub fn start() -> (McpAddWizard, String) {
    (
        McpAddWizard {
            name: String::new(),
            step: McpAddStep::AskName,
        },
        NAME_PROMPT.to_string(),
    )
}

/// Feeds one line of user input (already trimmed of the trailing newline —
/// callers pass the raw input-buffer text) into the wizard's current step.
pub fn advance(wizard: McpAddWizard, line: &str) -> Advance {
    let trimmed = line.trim();
    match wizard.step.clone() {
        McpAddStep::AskName => {
            if trimmed.is_empty() {
                return Advance::Invalid(
                    wizard,
                    format!("Server name cannot be empty. {NAME_PROMPT}"),
                );
            }
            Advance::Continue(
                McpAddWizard {
                    name: trimmed.to_string(),
                    step: McpAddStep::AskTransportChoice,
                },
                TRANSPORT_CHOICE_PROMPT.to_string(),
            )
        }
        McpAddStep::AskTransportChoice => match trimmed {
            "1" => Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskNpmPackage,
                },
                NPM_PACKAGE_PROMPT.to_string(),
            ),
            "2" => Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskPipxPackage,
                },
                PIPX_PACKAGE_PROMPT.to_string(),
            ),
            "3" => Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskCustomCommand,
                },
                COMMAND_PROMPT.to_string(),
            ),
            "4" => Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskHttpUrl,
                },
                HTTP_URL_PROMPT.to_string(),
            ),
            "5" => Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskSseUrl,
                },
                SSE_URL_PROMPT.to_string(),
            ),
            "6" => Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskWebsocketUrl,
                },
                WEBSOCKET_URL_PROMPT.to_string(),
            ),
            _ => {
                let msg = format!("'{trimmed}' isn't 1-6.\n{TRANSPORT_CHOICE_PROMPT}");
                Advance::Invalid(wizard, msg)
            }
        },
        McpAddStep::AskNpmPackage => {
            if trimmed.is_empty() {
                return Advance::Invalid(
                    wizard,
                    format!("Package name cannot be empty. {NPM_PACKAGE_PROMPT}"),
                );
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskNpmArgs {
                        package: trimmed.to_string(),
                        args: Vec::new(),
                    },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskNpmArgs { package, mut args } => {
            if trimmed.is_empty() {
                let mut full_args = vec!["-y".to_string(), package];
                full_args.extend(args);
                return Advance::Finalize(WizardOutput {
                    config: McpServerConfig {
                        name: wizard.name,
                        transport: McpTransportConfig::Stdio {
                            command: "npx".to_string(),
                            args: full_args,
                        },
                    },
                    pending_secret: None,
                });
            }
            args.push(trimmed.to_string());
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskNpmArgs { package, args },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskPipxPackage => {
            if trimmed.is_empty() {
                return Advance::Invalid(
                    wizard,
                    format!("Package name cannot be empty. {PIPX_PACKAGE_PROMPT}"),
                );
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskPipxArgs {
                        package: trimmed.to_string(),
                        args: Vec::new(),
                    },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskPipxArgs { package, mut args } => {
            if trimmed.is_empty() {
                let mut full_args = vec!["run".to_string(), package];
                full_args.extend(args);
                return Advance::Finalize(WizardOutput {
                    config: McpServerConfig {
                        name: wizard.name,
                        transport: McpTransportConfig::Stdio {
                            command: "pipx".to_string(),
                            args: full_args,
                        },
                    },
                    pending_secret: None,
                });
            }
            args.push(trimmed.to_string());
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskPipxArgs { package, args },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskCustomCommand => {
            if trimmed.is_empty() {
                return Advance::Invalid(
                    wizard,
                    format!("Command cannot be empty. {COMMAND_PROMPT}"),
                );
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskCustomArgs {
                        command: trimmed.to_string(),
                        args: Vec::new(),
                    },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskCustomArgs { command, mut args } => {
            if trimmed.is_empty() {
                return Advance::Finalize(WizardOutput {
                    config: McpServerConfig {
                        name: wizard.name,
                        transport: McpTransportConfig::Stdio { command, args },
                    },
                    pending_secret: None,
                });
            }
            args.push(trimmed.to_string());
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskCustomArgs { command, args },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskHttpUrl => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, format!("URL cannot be empty. {HTTP_URL_PROMPT}"));
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskHttpBearer {
                        url: trimmed.to_string(),
                    },
                },
                BEARER_PROMPT.to_string(),
            )
        }
        McpAddStep::AskHttpBearer { url } => {
            let (headers, pending_secret) = bearer_headers(&wizard.name, trimmed);
            Advance::Finalize(WizardOutput {
                config: McpServerConfig {
                    name: wizard.name,
                    transport: McpTransportConfig::Http { url, headers },
                },
                pending_secret,
            })
        }
        McpAddStep::AskSseUrl => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, format!("URL cannot be empty. {SSE_URL_PROMPT}"));
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskSseBearer {
                        url: trimmed.to_string(),
                    },
                },
                BEARER_PROMPT.to_string(),
            )
        }
        McpAddStep::AskSseBearer { url } => {
            let (headers, pending_secret) = bearer_headers(&wizard.name, trimmed);
            Advance::Finalize(WizardOutput {
                config: McpServerConfig {
                    name: wizard.name,
                    transport: McpTransportConfig::Sse { url, headers },
                },
                pending_secret,
            })
        }
        McpAddStep::AskWebsocketUrl => {
            if trimmed.is_empty() {
                return Advance::Invalid(
                    wizard,
                    format!("URL cannot be empty. {WEBSOCKET_URL_PROMPT}"),
                );
            }
            Advance::Finalize(WizardOutput {
                config: McpServerConfig {
                    name: wizard.name,
                    transport: McpTransportConfig::Websocket {
                        url: trimmed.to_string(),
                    },
                },
                pending_secret: None,
            })
        }
    }
}

/// Builds the header map and pending keyring secret for a bearer token
/// entered in the wizard. A blank token means no headers and no secret. The
/// secret name is derived from the (sanitized) server name so the header
/// reference and the keyring entry always agree.
fn bearer_headers(
    server_name: &str,
    token: &str,
) -> (HashMap<String, String>, Option<PendingSecret>) {
    if token.is_empty() {
        return (HashMap::new(), None);
    }
    let secret_name = format!("mcp-{}", sanitize_secret_name(server_name));
    let headers = HashMap::from([(
        "Authorization".to_string(),
        format!("Bearer ${{keyring:{secret_name}}}"),
    )]);
    (
        headers,
        Some(PendingSecret {
            name: secret_name,
            value: token.to_string(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_secret_debug_redacts_value() {
        let pending = PendingSecret {
            name: "my-secret".to_string(),
            value: "tok".to_string(),
        };
        let debug = format!("{pending:?}");
        assert!(!debug.contains("tok"));
        assert!(debug.contains("my-secret"));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn empty_name_is_invalid_and_keeps_asking_for_a_name() {
        let (wizard, _) = start();
        match advance(wizard, "") {
            Advance::Invalid(w, msg) => {
                assert_eq!(w.step, McpAddStep::AskName);
                assert!(msg.contains("cannot be empty"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn name_then_bad_transport_digit_is_invalid() {
        let (wizard, _) = start();
        let Advance::Continue(wizard, _) = advance(wizard, "my-server") else {
            panic!("expected Continue")
        };
        match advance(wizard, "9") {
            Advance::Invalid(w, msg) => {
                assert_eq!(w.step, McpAddStep::AskTransportChoice);
                assert!(msg.contains("isn't 1-6"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    fn name_and_transport(digit: &str) -> McpAddWizard {
        let (wizard, _) = start();
        let Advance::Continue(wizard, _) = advance(wizard, "my-server") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, digit) else {
            panic!("expected Continue")
        };
        wizard
    }

    #[test]
    fn npm_branch_finalizes_with_dash_y_package_and_extra_args() {
        let wizard = name_and_transport("1");
        let Advance::Continue(wizard, _) =
            advance(wizard, "@modelcontextprotocol/server-filesystem")
        else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, "/tmp") else {
            panic!("expected Continue")
        };
        match advance(wizard, "") {
            Advance::Finalize(output) => {
                assert_eq!(output.config.name, "my-server");
                assert_eq!(
                    output.config.transport,
                    McpTransportConfig::Stdio {
                        command: "npx".into(),
                        args: vec![
                            "-y".into(),
                            "@modelcontextprotocol/server-filesystem".into(),
                            "/tmp".into()
                        ],
                    }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn pipx_branch_finalizes_with_run_package_and_no_extra_args() {
        let wizard = name_and_transport("2");
        let Advance::Continue(wizard, _) = advance(wizard, "some-pipx-tool") else {
            panic!("expected Continue")
        };
        match advance(wizard, "") {
            Advance::Finalize(output) => {
                assert_eq!(
                    output.config.transport,
                    McpTransportConfig::Stdio {
                        command: "pipx".into(),
                        args: vec!["run".into(), "some-pipx-tool".into()],
                    }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn custom_stdio_branch_finalizes_with_raw_command_and_args() {
        let wizard = name_and_transport("3");
        let Advance::Continue(wizard, _) = advance(wizard, "my-mcp-binary") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, "--flag") else {
            panic!("expected Continue")
        };
        match advance(wizard, "") {
            Advance::Finalize(output) => {
                assert_eq!(
                    output.config.transport,
                    McpTransportConfig::Stdio {
                        command: "my-mcp-binary".into(),
                        args: vec!["--flag".into()]
                    }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn http_branch_with_blank_token_finalizes_with_no_headers_and_no_secret() {
        let wizard = name_and_transport("4");
        let Advance::Continue(wizard, prompt) = advance(wizard, "http://localhost:9000/mcp") else {
            panic!("expected Continue to the bearer prompt")
        };
        assert!(prompt.contains("Bearer token"));
        match advance(wizard, "") {
            Advance::Finalize(output) => {
                assert_eq!(output.pending_secret, None);
                assert_eq!(
                    output.config.transport,
                    McpTransportConfig::Http {
                        url: "http://localhost:9000/mcp".into(),
                        headers: HashMap::new()
                    }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn http_branch_with_token_finalizes_with_keyring_reference_and_pending_secret() {
        let wizard = name_and_transport("4");
        let Advance::Continue(wizard, _) = advance(wizard, "http://localhost:9000/mcp") else {
            panic!("expected Continue")
        };
        match advance(wizard, "tok-secret-1") {
            Advance::Finalize(output) => {
                assert_eq!(
                    output.pending_secret,
                    Some(PendingSecret {
                        name: "mcp-my-server".into(),
                        value: "tok-secret-1".into(),
                    })
                );
                assert_eq!(
                    output.config.transport,
                    McpTransportConfig::Http {
                        url: "http://localhost:9000/mcp".into(),
                        headers: HashMap::from([(
                            "Authorization".to_string(),
                            "Bearer ${keyring:mcp-my-server}".to_string(),
                        )]),
                    }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn sse_branch_with_blank_token_finalizes_with_no_headers_and_no_secret() {
        let wizard = name_and_transport("5");
        let Advance::Continue(wizard, prompt) = advance(wizard, "http://localhost:9002/sse") else {
            panic!("expected Continue to the bearer prompt")
        };
        assert!(prompt.contains("Bearer token"));
        match advance(wizard, "") {
            Advance::Finalize(output) => {
                assert_eq!(output.pending_secret, None);
                assert_eq!(
                    output.config.transport,
                    McpTransportConfig::Sse {
                        url: "http://localhost:9002/sse".into(),
                        headers: HashMap::new()
                    }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn sse_branch_with_token_finalizes_with_keyring_reference() {
        let wizard = name_and_transport("5");
        let Advance::Continue(wizard, prompt) = advance(wizard, "http://localhost:9002/sse") else {
            panic!("expected Continue to the bearer prompt")
        };
        assert!(prompt.contains("Bearer token"));
        match advance(wizard, "tok-sse") {
            Advance::Finalize(output) => {
                let McpTransportConfig::Sse { headers, .. } = &output.config.transport else {
                    panic!("expected Sse transport");
                };
                assert_eq!(headers["Authorization"], "Bearer ${keyring:mcp-my-server}");
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn secret_name_is_sanitized_from_the_server_name() {
        let (wizard, _) = start();
        let Advance::Continue(wizard, _) = advance(wizard, "my tools!") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, "4") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, "http://localhost:9000/mcp") else {
            panic!("expected Continue")
        };
        match advance(wizard, "tok") {
            Advance::Finalize(output) => {
                assert_eq!(output.pending_secret.unwrap().name, "mcp-my-tools-");
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn websocket_branch_finalizes_directly_from_the_url() {
        let wizard = name_and_transport("6");
        match advance(wizard, "ws://localhost:9001/mcp") {
            Advance::Finalize(output) => {
                assert_eq!(
                    output.config.transport,
                    McpTransportConfig::Websocket {
                        url: "ws://localhost:9001/mcp".into()
                    }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn blank_url_is_invalid_for_http_sse_and_websocket() {
        for digit in ["4", "5", "6"] {
            let wizard = name_and_transport(digit);
            match advance(wizard, "") {
                Advance::Invalid(_, msg) => assert!(msg.contains("cannot be empty")),
                other => panic!("expected Invalid for digit {digit}, got {other:?}"),
            }
        }
    }
}
