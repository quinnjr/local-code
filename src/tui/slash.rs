/// One recognized slash command, already split into its command word and
/// remaining argument text. `parse_slash_command` is the single place that
/// knows the full v1 command list (spec section 6); `App`'s `Enter` handler
/// matches on this enum instead of re-parsing strings inline.
use crate::agent::effort::ReasoningEffort;

/// The argument to `/effort <arg>`: a level, or `off`/`none`/`default` to
/// drop the session override and fall back to the connection's default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffortArg {
    Level(ReasoningEffort),
    Off,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    Model,
    /// `/effort` (menu) or `/effort low|medium|high|off`.
    Effort {
        level: Option<EffortArg>,
    },
    ConnectionsList,
    ConnectionsRemove {
        name: String,
    },
    ConnectionsAdd,
    McpList,
    McpRemove {
        name: String,
    },
    McpAdd,
    Init,
    Permissions,
    Compact,
    Resume,
    Clear,
    Help,
    Unknown {
        raw: String,
    },
}

/// Parses `input` as a slash command if it starts with `/` (after
/// trimming), returning `None` for anything else (a normal prompt). Unknown
/// `/word` input still parses to `Some(SlashCommand::Unknown { .. })` rather
/// than `None`, so the caller can show a clear "unrecognized command" notice
/// instead of silently sending `/typo` to the model as a prompt.
pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut parts = trimmed[1..].split_whitespace();
    let command = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();

    Some(match command {
        "model" => SlashCommand::Model,
        "effort" => match rest.as_slice() {
            [] => SlashCommand::Effort { level: None },
            ["off"] | ["none"] | ["default"] => SlashCommand::Effort {
                level: Some(EffortArg::Off),
            },
            [level] => match level.parse::<ReasoningEffort>() {
                Ok(level) => SlashCommand::Effort {
                    level: Some(EffortArg::Level(level)),
                },
                Err(_) => SlashCommand::Unknown {
                    raw: trimmed.to_string(),
                },
            },
            _ => SlashCommand::Unknown {
                raw: trimmed.to_string(),
            },
        },
        "connections" => match rest.as_slice() {
            ["list"] | [] => SlashCommand::ConnectionsList,
            ["remove", name] => SlashCommand::ConnectionsRemove {
                name: name.to_string(),
            },
            ["add"] => SlashCommand::ConnectionsAdd,
            _ => SlashCommand::Unknown {
                raw: trimmed.to_string(),
            },
        },
        "mcp" => match rest.as_slice() {
            ["list"] | [] => SlashCommand::McpList,
            ["remove", name] => SlashCommand::McpRemove {
                name: name.to_string(),
            },
            ["add"] => SlashCommand::McpAdd,
            _ => SlashCommand::Unknown {
                raw: trimmed.to_string(),
            },
        },
        "init" => SlashCommand::Init,
        "permissions" => SlashCommand::Permissions,
        "compact" => SlashCommand::Compact,
        "resume" => SlashCommand::Resume,
        "clear" => SlashCommand::Clear,
        "help" => SlashCommand::Help,
        _ => SlashCommand::Unknown {
            raw: trimmed.to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_slash_input_parses_to_none() {
        assert_eq!(parse_slash_command("fix the bug"), None);
    }

    #[test]
    fn recognizes_every_v1_command() {
        assert_eq!(parse_slash_command("/model"), Some(SlashCommand::Model));
        assert_eq!(
            parse_slash_command("/connections"),
            Some(SlashCommand::ConnectionsList)
        );
        assert_eq!(
            parse_slash_command("/connections list"),
            Some(SlashCommand::ConnectionsList)
        );
        assert_eq!(
            parse_slash_command("/connections remove local-vllm"),
            Some(SlashCommand::ConnectionsRemove {
                name: "local-vllm".into()
            })
        );
        assert_eq!(
            parse_slash_command("/connections add"),
            Some(SlashCommand::ConnectionsAdd)
        );
        assert_eq!(parse_slash_command("/init"), Some(SlashCommand::Init));
        assert_eq!(
            parse_slash_command("/permissions"),
            Some(SlashCommand::Permissions)
        );
        assert_eq!(parse_slash_command("/compact"), Some(SlashCommand::Compact));
        assert_eq!(parse_slash_command("/resume"), Some(SlashCommand::Resume));
        assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::Clear));
        assert_eq!(parse_slash_command("/help"), Some(SlashCommand::Help));
    }

    #[test]
    fn recognizes_effort_commands() {
        assert_eq!(
            parse_slash_command("/effort"),
            Some(SlashCommand::Effort { level: None })
        );
        assert_eq!(
            parse_slash_command("/effort High"),
            Some(SlashCommand::Effort {
                level: Some(EffortArg::Level(ReasoningEffort::High))
            })
        );
        assert_eq!(
            parse_slash_command("/effort med"),
            Some(SlashCommand::Effort {
                level: Some(EffortArg::Level(ReasoningEffort::Medium))
            })
        );
        for word in ["off", "none", "default"] {
            assert_eq!(
                parse_slash_command(&format!("/effort {word}")),
                Some(SlashCommand::Effort {
                    level: Some(EffortArg::Off)
                })
            );
        }
        assert_eq!(
            parse_slash_command("/effort extreme"),
            Some(SlashCommand::Unknown {
                raw: "/effort extreme".into()
            })
        );
        assert_eq!(
            parse_slash_command("/effort low high"),
            Some(SlashCommand::Unknown {
                raw: "/effort low high".into()
            })
        );
    }

    #[test]
    fn unrecognized_slash_word_is_unknown_not_none() {
        assert_eq!(
            parse_slash_command("/typo"),
            Some(SlashCommand::Unknown {
                raw: "/typo".into()
            })
        );
    }

    #[test]
    fn malformed_connections_subcommand_is_unknown() {
        assert_eq!(
            parse_slash_command("/connections bogus"),
            Some(SlashCommand::Unknown {
                raw: "/connections bogus".into()
            })
        );
    }

    #[test]
    fn leading_and_trailing_whitespace_is_tolerated() {
        assert_eq!(parse_slash_command("  /help  "), Some(SlashCommand::Help));
    }

    #[test]
    fn recognizes_mcp_commands() {
        assert_eq!(parse_slash_command("/mcp"), Some(SlashCommand::McpList));
        assert_eq!(
            parse_slash_command("/mcp list"),
            Some(SlashCommand::McpList)
        );
        assert_eq!(
            parse_slash_command("/mcp remove filesystem"),
            Some(SlashCommand::McpRemove {
                name: "filesystem".into()
            })
        );
        assert_eq!(parse_slash_command("/mcp add"), Some(SlashCommand::McpAdd));
    }

    #[test]
    fn malformed_mcp_subcommand_is_unknown() {
        assert_eq!(
            parse_slash_command("/mcp bogus"),
            Some(SlashCommand::Unknown {
                raw: "/mcp bogus".into()
            })
        );
    }
}
