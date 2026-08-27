use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// How much reasoning ("thinking") a model should spend per response — the
/// three levels every OpenAI-compatible server that supports the field
/// accepts (`reasoning_effort: "low" | "medium" | "high"`).
///
/// Only `openai-compatible` connections can carry it today: daimon 0.23's
/// `Ollama` and `OpenRouter` builders expose no extra-field hook, so
/// [`crate::agent::provider::build_model`] ignores the setting there and the
/// TUI/CLI tell the user so (see [`supports_effort`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    pub const ALL: [ReasoningEffort; 3] = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ];

    /// The wire value (`"low"`/`"medium"`/`"high"`) — also what `/effort`
    /// prints and parses.
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = String;

    /// Case-insensitive; accepts the common `med` shorthand for `medium`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(ReasoningEffort::Low),
            "medium" | "med" => Ok(ReasoningEffort::Medium),
            "high" => Ok(ReasoningEffort::High),
            other => Err(format!(
                "unknown effort level '{other}' (expected low, medium, or high)"
            )),
        }
    }
}

/// Whether `build_model` can actually send a reasoning effort for this
/// provider kind. Kept next to the enum so every "effort is ignored here"
/// notice (TUI, CLI) derives from the same predicate as the builder.
pub fn supports_effort(provider: crate::config::connection::ProviderKind) -> bool {
    matches!(
        provider,
        crate::config::connection::ProviderKind::OpenAiCompatible
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::connection::ProviderKind;

    #[test]
    fn parses_case_insensitively_with_med_shorthand() {
        assert_eq!("LOW".parse::<ReasoningEffort>(), Ok(ReasoningEffort::Low));
        assert_eq!(
            " med ".parse::<ReasoningEffort>(),
            Ok(ReasoningEffort::Medium)
        );
        assert_eq!(
            "Medium".parse::<ReasoningEffort>(),
            Ok(ReasoningEffort::Medium)
        );
        assert_eq!("high".parse::<ReasoningEffort>(), Ok(ReasoningEffort::High));
        assert!("extreme".parse::<ReasoningEffort>().is_err());
    }

    #[test]
    fn display_round_trips_through_from_str() {
        for level in ReasoningEffort::ALL {
            assert_eq!(level.to_string().parse::<ReasoningEffort>(), Ok(level));
        }
    }

    #[test]
    fn serde_uses_lowercase_wire_values() {
        assert_eq!(
            serde_json::to_string(&ReasoningEffort::High).unwrap(),
            "\"high\""
        );
        assert_eq!(
            serde_json::from_str::<ReasoningEffort>("\"medium\"").unwrap(),
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn only_openai_compatible_supports_effort() {
        assert!(supports_effort(ProviderKind::OpenAiCompatible));
        assert!(!supports_effort(ProviderKind::Ollama));
        assert!(!supports_effort(ProviderKind::OpenRouter));
    }
}
