// src/tui/mcp_wizard.rs

use std::collections::HashMap;

use crate::config::mcp_servers::{McpServerConfig, McpTransportConfig};

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
    AskSseUrl,
    AskWebsocketUrl,
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpAddWizard {
    pub name: String,
    pub step: McpAddStep,
}

/// The result of feeding one line of input into `advance`.
#[derive(Debug, PartialEq)]
pub enum Advance {
    /// Move to the next step; the `String` is the prompt to show for it.
    Continue(McpAddWizard, String),
    /// All required fields are collected — build the server config.
    Finalize(McpServerConfig),
    /// The line was invalid for the current step (e.g. blank where a value
    /// is required); re-prompt with the same wizard state (the `String` is
    /// the message to show, ending with a repeat of the original prompt).
    Invalid(McpAddWizard, String),
}

const TRANSPORT_CHOICE_PROMPT: &str = "Choose a transport (press the digit key):\n\
     1) npm package\n2) pipx package\n3) custom stdio command\n4) HTTP URL\n5) SSE URL\n6) WebSocket URL";
const ARGS_PROMPT: &str = "Extra args, one per line (blank line to finish): ";

/// Starts a fresh wizard, returning its initial state and the first prompt
/// to show the user.
pub fn start() -> (McpAddWizard, String) {
    (
        McpAddWizard { name: String::new(), step: McpAddStep::AskName },
        "Server name: ".to_string(),
    )
}

/// Feeds one line of user input (already trimmed of the trailing newline —
/// callers pass the raw input-buffer text) into the wizard's current step.
pub fn advance(wizard: McpAddWizard, line: &str) -> Advance {
    let trimmed = line.trim();
    match wizard.step.clone() {
        McpAddStep::AskName => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, "Server name cannot be empty. Server name: ".to_string());
            }
            Advance::Continue(
                McpAddWizard { name: trimmed.to_string(), step: McpAddStep::AskTransportChoice },
                TRANSPORT_CHOICE_PROMPT.to_string(),
            )
        }
        McpAddStep::AskTransportChoice => match trimmed {
            "1" => Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskNpmPackage },
                "npm package name: ".to_string(),
            ),
            "2" => Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskPipxPackage },
                "pipx package name: ".to_string(),
            ),
            "3" => Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskCustomCommand },
                "Command: ".to_string(),
            ),
            "4" => Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskHttpUrl },
                "HTTP URL: ".to_string(),
            ),
            "5" => Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskSseUrl },
                "SSE URL: ".to_string(),
            ),
            "6" => Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskWebsocketUrl },
                "WebSocket URL: ".to_string(),
            ),
            _ => {
                let msg = format!("'{trimmed}' isn't 1-6.\n{TRANSPORT_CHOICE_PROMPT}");
                Advance::Invalid(wizard, msg)
            }
        },
        McpAddStep::AskNpmPackage => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, "Package name cannot be empty. npm package name: ".to_string());
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskNpmArgs { package: trimmed.to_string(), args: Vec::new() },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskNpmArgs { package, mut args } => {
            if trimmed.is_empty() {
                let mut full_args = vec!["-y".to_string(), package];
                full_args.extend(args);
                return Advance::Finalize(McpServerConfig {
                    name: wizard.name,
                    transport: McpTransportConfig::Stdio { command: "npx".to_string(), args: full_args },
                });
            }
            args.push(trimmed.to_string());
            Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskNpmArgs { package, args } },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskPipxPackage => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, "Package name cannot be empty. pipx package name: ".to_string());
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskPipxArgs { package: trimmed.to_string(), args: Vec::new() },
                },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskPipxArgs { package, mut args } => {
            if trimmed.is_empty() {
                let mut full_args = vec!["run".to_string(), package];
                full_args.extend(args);
                return Advance::Finalize(McpServerConfig {
                    name: wizard.name,
                    transport: McpTransportConfig::Stdio { command: "pipx".to_string(), args: full_args },
                });
            }
            args.push(trimmed.to_string());
            Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskPipxArgs { package, args } },
                ARGS_PROMPT.to_string(),
            )
        }
        McpAddStep::AskCustomCommand => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, "Command cannot be empty. Command: ".to_string());
            }
            Advance::Continue(
                McpAddWizard {
                    name: wizard.name,
                    step: McpAddStep::AskCustomArgs { command: trimmed.to_string(), args: Vec::new() },
                },
                "Args, one per line (blank line to finish): ".to_string(),
            )
        }
        McpAddStep::AskCustomArgs { command, mut args } => {
            if trimmed.is_empty() {
                return Advance::Finalize(McpServerConfig {
                    name: wizard.name,
                    transport: McpTransportConfig::Stdio { command, args },
                });
            }
            args.push(trimmed.to_string());
            Advance::Continue(
                McpAddWizard { name: wizard.name, step: McpAddStep::AskCustomArgs { command, args } },
                "Args, one per line (blank line to finish): ".to_string(),
            )
        }
        McpAddStep::AskHttpUrl => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, "URL cannot be empty. HTTP URL: ".to_string());
            }
            Advance::Finalize(McpServerConfig {
                name: wizard.name,
                transport: McpTransportConfig::Http { url: trimmed.to_string(), headers: HashMap::new() },
            })
        }
        McpAddStep::AskSseUrl => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, "URL cannot be empty. SSE URL: ".to_string());
            }
            Advance::Finalize(McpServerConfig {
                name: wizard.name,
                transport: McpTransportConfig::Sse { url: trimmed.to_string(), headers: HashMap::new() },
            })
        }
        McpAddStep::AskWebsocketUrl => {
            if trimmed.is_empty() {
                return Advance::Invalid(wizard, "URL cannot be empty. WebSocket URL: ".to_string());
            }
            Advance::Finalize(McpServerConfig {
                name: wizard.name,
                transport: McpTransportConfig::Websocket { url: trimmed.to_string() },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let Advance::Continue(wizard, _) = advance(wizard, "my-server") else { panic!("expected Continue") };
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
        let Advance::Continue(wizard, _) = advance(wizard, "my-server") else { panic!("expected Continue") };
        let Advance::Continue(wizard, _) = advance(wizard, digit) else { panic!("expected Continue") };
        wizard
    }

    #[test]
    fn npm_branch_finalizes_with_dash_y_package_and_extra_args() {
        let wizard = name_and_transport("1");
        let Advance::Continue(wizard, _) = advance(wizard, "@modelcontextprotocol/server-filesystem") else {
            panic!("expected Continue")
        };
        let Advance::Continue(wizard, _) = advance(wizard, "/tmp") else { panic!("expected Continue") };
        match advance(wizard, "") {
            Advance::Finalize(config) => {
                assert_eq!(config.name, "my-server");
                assert_eq!(
                    config.transport,
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
        let Advance::Continue(wizard, _) = advance(wizard, "some-pipx-tool") else { panic!("expected Continue") };
        match advance(wizard, "") {
            Advance::Finalize(config) => {
                assert_eq!(
                    config.transport,
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
        let Advance::Continue(wizard, _) = advance(wizard, "my-mcp-binary") else { panic!("expected Continue") };
        let Advance::Continue(wizard, _) = advance(wizard, "--flag") else { panic!("expected Continue") };
        match advance(wizard, "") {
            Advance::Finalize(config) => {
                assert_eq!(
                    config.transport,
                    McpTransportConfig::Stdio { command: "my-mcp-binary".into(), args: vec!["--flag".into()] }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn http_branch_finalizes_directly_from_the_url_with_no_headers() {
        let wizard = name_and_transport("4");
        match advance(wizard, "http://localhost:9000/mcp") {
            Advance::Finalize(config) => {
                assert_eq!(
                    config.transport,
                    McpTransportConfig::Http { url: "http://localhost:9000/mcp".into(), headers: HashMap::new() }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn sse_branch_finalizes_directly_from_the_url() {
        let wizard = name_and_transport("5");
        match advance(wizard, "http://localhost:9002/sse") {
            Advance::Finalize(config) => {
                assert_eq!(
                    config.transport,
                    McpTransportConfig::Sse { url: "http://localhost:9002/sse".into(), headers: HashMap::new() }
                );
            }
            other => panic!("expected Finalize, got {other:?}"),
        }
    }

    #[test]
    fn websocket_branch_finalizes_directly_from_the_url() {
        let wizard = name_and_transport("6");
        match advance(wizard, "ws://localhost:9001/mcp") {
            Advance::Finalize(config) => {
                assert_eq!(config.transport, McpTransportConfig::Websocket { url: "ws://localhost:9001/mcp".into() });
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
