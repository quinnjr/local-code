// src/tui/slash.rs

/// One recognized slash command, already split into its command word and
/// remaining argument text. `parse_slash_command` is the single place that
/// knows the full v1 command list (spec section 6); `App`'s `Enter` handler
/// matches on this enum instead of re-parsing strings inline.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    Model,
    ConnectionsList,
    ConnectionsRemove { name: String },
    ConnectionsAddUnsupported,
    Init,
    Permissions,
    Compact,
    Resume,
    Clear,
    Help,
    Unknown { raw: String },
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
        "connections" => match rest.as_slice() {
            ["list"] | [] => SlashCommand::ConnectionsList,
            ["remove", name] => SlashCommand::ConnectionsRemove { name: name.to_string() },
            ["add"] => SlashCommand::ConnectionsAddUnsupported,
            _ => SlashCommand::Unknown { raw: trimmed.to_string() },
        },
        "init" => SlashCommand::Init,
        "permissions" => SlashCommand::Permissions,
        "compact" => SlashCommand::Compact,
        "resume" => SlashCommand::Resume,
        "clear" => SlashCommand::Clear,
        "help" => SlashCommand::Help,
        _ => SlashCommand::Unknown { raw: trimmed.to_string() },
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
        assert_eq!(parse_slash_command("/connections"), Some(SlashCommand::ConnectionsList));
        assert_eq!(parse_slash_command("/connections list"), Some(SlashCommand::ConnectionsList));
        assert_eq!(
            parse_slash_command("/connections remove local-vllm"),
            Some(SlashCommand::ConnectionsRemove { name: "local-vllm".into() })
        );
        assert_eq!(parse_slash_command("/connections add"), Some(SlashCommand::ConnectionsAddUnsupported));
        assert_eq!(parse_slash_command("/init"), Some(SlashCommand::Init));
        assert_eq!(parse_slash_command("/permissions"), Some(SlashCommand::Permissions));
        assert_eq!(parse_slash_command("/compact"), Some(SlashCommand::Compact));
        assert_eq!(parse_slash_command("/resume"), Some(SlashCommand::Resume));
        assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::Clear));
        assert_eq!(parse_slash_command("/help"), Some(SlashCommand::Help));
    }

    #[test]
    fn unrecognized_slash_word_is_unknown_not_none() {
        assert_eq!(
            parse_slash_command("/typo"),
            Some(SlashCommand::Unknown { raw: "/typo".into() })
        );
    }

    #[test]
    fn malformed_connections_subcommand_is_unknown() {
        assert_eq!(
            parse_slash_command("/connections bogus"),
            Some(SlashCommand::Unknown { raw: "/connections bogus".into() })
        );
    }

    #[test]
    fn leading_and_trailing_whitespace_is_tolerated() {
        assert_eq!(parse_slash_command("  /help  "), Some(SlashCommand::Help));
    }
}
