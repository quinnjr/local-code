pub mod connections;
pub mod memory;

use crate::agent::headless::run_headless;
use crate::config::paths::Paths;
use crate::permissions::types::PermissionTier;
use clap::{Parser, Subcommand, ValueEnum};
use std::io::{stdin, stdout, IsTerminal};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "local-code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Run one prompt to completion headlessly and print the final response.
    #[arg(short = 'p', long = "prompt")]
    pub prompt: Option<String>,

    /// Which configured connection to use for `-p` (required if more than one is configured).
    #[arg(long)]
    pub connection: Option<String>,

    /// Overrides the permission tier for `-p` (defaults to full-auto in headless mode).
    #[arg(long = "permission-mode", value_enum)]
    pub permission_mode: Option<PermissionModeArg>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage LLM connections (add/list/remove)
    Connections {
        #[command(subcommand)]
        action: ConnectionsAction,
    },
    /// Inspect cross-session memory (search/core/add)
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
}

#[derive(Subcommand)]
pub enum ConnectionsAction {
    Add,
    List,
    Remove { name: String },
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Keyword-search the buffer, daily files, recent.md, and archive.md
    Search { query: String },
    /// Print the always-loaded core-memories.md file in full
    Core,
    /// Append a manual entry to the short-term buffer
    Add { text: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PermissionModeArg {
    Ask,
    AutoAcceptEdits,
    FullAuto,
}

impl PermissionModeArg {
    pub fn into_tier(self) -> PermissionTier {
        match self {
            PermissionModeArg::Ask => PermissionTier::Ask,
            PermissionModeArg::AutoAcceptEdits => PermissionTier::AutoAcceptEdits,
            PermissionModeArg::FullAuto => PermissionTier::FullAuto,
        }
    }
}

/// Returns whether the given (possibly-absent) permission tier override would
/// require an interactive terminal to answer permission prompts. Pure decision
/// logic, kept separate from the actual TTY-detection so it can be unit-tested.
fn requires_interactive_stdin(tier: Option<PermissionTier>) -> bool {
    matches!(
        tier,
        Some(PermissionTier::Ask) | Some(PermissionTier::AutoAcceptEdits)
    )
}

pub async fn run(cli: Cli, project_root: PathBuf) -> anyhow::Result<()> {
    let paths = Paths::resolve(&project_root)?;

    if let Some(prompt) = cli.prompt.as_deref() {
        let tier_override = cli.permission_mode.map(PermissionModeArg::into_tier);
        if requires_interactive_stdin(tier_override) && !stdin().is_terminal() {
            anyhow::bail!(
                "--permission-mode ask/auto-accept-edits requires an interactive terminal to answer \
                 permission prompts, but stdin is not a TTY (e.g. running under a script or pipe). \
                 Use --permission-mode full-auto for non-interactive/scripted invocations instead."
            );
        }
        let final_text = run_headless(
            &paths,
            &project_root,
            cli.connection.as_deref(),
            tier_override,
            prompt,
        )
        .await?;
        println!("{final_text}");
        return Ok(());
    }

    match cli.command {
        Some(Command::Connections { action }) => match action {
            ConnectionsAction::Add => {
                connections::add(&paths, stdin().lock(), stdout())?;
            }
            ConnectionsAction::List => {
                connections::list(&paths, stdout())?;
            }
            ConnectionsAction::Remove { name } => {
                connections::remove(&paths, &name, stdout())?;
            }
        },
        Some(Command::Memory { action }) => match action {
            MemoryAction::Search { query } => {
                memory::search_command(&paths, &query, stdout())?;
            }
            MemoryAction::Core => {
                memory::core_command(&paths, stdout())?;
            }
            MemoryAction::Add { text } => {
                memory::add_command(&paths, &text, stdout())?;
            }
        },
        None => {
            crate::tui::run_tui(
                &paths,
                &project_root,
                cli.connection.as_deref(),
                cli.permission_mode.map(PermissionModeArg::into_tier),
                None,
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod headless_cli_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_prompt_flag() {
        let cli = Cli::parse_from(["local-code", "-p", "fix the bug"]);
        assert_eq!(cli.prompt.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn parses_connection_and_permission_mode_flags() {
        let cli = Cli::parse_from([
            "local-code",
            "-p",
            "hello",
            "--connection",
            "local-vllm",
            "--permission-mode",
            "ask",
        ]);
        assert_eq!(cli.connection.as_deref(), Some("local-vllm"));
        assert_eq!(cli.permission_mode, Some(PermissionModeArg::Ask));
    }

    #[test]
    fn permission_mode_maps_to_permission_tier() {
        assert_eq!(
            PermissionModeArg::Ask.into_tier(),
            crate::permissions::types::PermissionTier::Ask
        );
        assert_eq!(
            PermissionModeArg::AutoAcceptEdits.into_tier(),
            crate::permissions::types::PermissionTier::AutoAcceptEdits
        );
        assert_eq!(
            PermissionModeArg::FullAuto.into_tier(),
            crate::permissions::types::PermissionTier::FullAuto
        );
    }

    #[test]
    fn no_prompt_flag_leaves_prompt_none() {
        let cli = Cli::parse_from(["local-code", "connections", "list"]);
        assert_eq!(cli.prompt, None);
    }

    #[test]
    fn requires_interactive_stdin_true_for_ask_and_auto_accept_edits() {
        assert!(requires_interactive_stdin(Some(PermissionTier::Ask)));
        assert!(requires_interactive_stdin(Some(
            PermissionTier::AutoAcceptEdits
        )));
    }

    #[test]
    fn requires_interactive_stdin_false_for_full_auto_and_none() {
        assert!(!requires_interactive_stdin(Some(PermissionTier::FullAuto)));
        assert!(!requires_interactive_stdin(None));
    }
}
