pub mod connections;
pub mod mcp;
pub mod memory;
pub mod skills;

use crate::agent::headless::run_headless;
use crate::config::paths::Paths;
use crate::permissions::types::PermissionTier;
use clap::{Parser, Subcommand, ValueEnum};
use std::io::{IsTerminal, stdin, stdout};
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

    /// Resume a previous session for this project: lists recent sessions and
    /// prompts for a choice (reading a line from stdin), or reopens the most
    /// recent one automatically if exactly one exists.
    #[arg(long)]
    pub resume: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage LLM connections (add/list/remove)
    Connections {
        #[command(subcommand)]
        action: ConnectionsAction,
    },
    /// Manage MCP servers (list/remove; add is TUI-only via /mcp add)
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Inspect cross-session memory (search/core/add)
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Manage skills (install/list/remove/update from GitHub)
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum McpAction {
    Add,
    List,
    Remove { name: String },
}

#[derive(Subcommand, Debug)]
pub enum ConnectionsAction {
    Add,
    List,
    Remove { name: String },
}

#[derive(Subcommand, Debug)]
pub enum MemoryAction {
    /// Keyword-search the buffer, daily files, recent.md, and archive.md
    Search { query: String },
    /// Print the always-loaded core-memories.md file in full
    Core,
    /// Append a manual entry to the short-term buffer
    Add { text: String },
}

#[derive(Subcommand, Debug)]
pub enum SkillsAction {
    /// Install a skill from GitHub: owner/repo[/path][@ref]
    Install {
        spec: String,
        /// Install into the global (user-level) scope instead of this project
        #[arg(long)]
        global: bool,
        /// Override the derived skill name
        #[arg(long)]
        name: Option<String>,
    },
    /// List installed skills across both scopes
    List,
    /// Remove an installed skill
    Remove {
        name: String,
        #[arg(long)]
        global: bool,
    },
    /// Re-fetch a skill (or all skills in scope) if its pinned ref has moved
    Update {
        name: Option<String>,
        #[arg(long)]
        global: bool,
    },
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
        Some(Command::Mcp { action }) => match action {
            McpAction::Add => {
                mcp::add_unsupported(stdout())?;
            }
            McpAction::List => {
                mcp::list(&paths, stdout())?;
            }
            McpAction::Remove { name } => {
                mcp::remove(&paths, &name, stdout())?;
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
        Some(Command::Skills { action }) => match action {
            SkillsAction::Install { spec, global, name } => {
                skills::install(&paths, &spec, global, name.as_deref(), stdout()).await?;
            }
            SkillsAction::List => {
                skills::list(&paths, stdout())?;
            }
            SkillsAction::Remove { name, global } => {
                skills::remove(&paths, &name, global, stdout())?;
            }
            SkillsAction::Update { name, global } => {
                skills::update(&paths, name.as_deref(), global, stdout()).await?;
            }
        },
        None => {
            let resume = if cli.resume {
                let sessions =
                    crate::session::store::list_sessions(&paths.user_state_dir, &project_root)?;
                let chosen = select_session_to_resume(&sessions, stdin().lock(), stdout())?;
                match chosen {
                    Some(summary) => {
                        let session = crate::session::store::load_session(&summary.path)?;
                        Some(crate::tui::ResumedSession {
                            session_path: summary.path,
                            entries: session.entries,
                            messages: session.messages,
                            tier: session.tier,
                            connection_name: session.connection_name,
                            model_name: session.model_name,
                            created_at: session.created_at,
                        })
                    }
                    None => None,
                }
            } else {
                None
            };

            crate::tui::run_tui(
                &paths,
                &project_root,
                cli.connection.as_deref(),
                cli.permission_mode.map(PermissionModeArg::into_tier),
                resume,
            )
            .await?;
        }
    }
    Ok(())
}

use crate::session::types::SessionSummary;
use std::io::{BufRead, Write};

/// Resolves which session to resume from a listing, generic over
/// `BufRead`/`Write` for the same testability reason Phase 1's `connections
/// add` wizard is (`src/cli/connections.rs`). If exactly one session exists,
/// it's returned without prompting ("reopens the most recent if
/// unambiguous", per this plan's Architecture section); a blank line at the
/// prompt also selects the most recent (index 1) as a convenient default.
pub fn select_session_to_resume<R: BufRead, W: Write>(
    sessions: &[SessionSummary],
    mut input: R,
    mut out: W,
) -> anyhow::Result<Option<SessionSummary>> {
    if sessions.is_empty() {
        writeln!(out, "No previous sessions found for this project.")?;
        return Ok(None);
    }
    if sessions.len() == 1 {
        writeln!(
            out,
            "Resuming the only previous session ({}).",
            sessions[0].updated_at
        )?;
        return Ok(Some(sessions[0].clone()));
    }

    writeln!(out, "Previous sessions for this project:")?;
    for (i, s) in sessions.iter().enumerate() {
        writeln!(
            out,
            "  {}) {} · {} · {}{}",
            i + 1,
            s.updated_at,
            s.connection_name,
            s.model_name,
            s.first_user_turn_preview
                .as_ref()
                .map(|p| format!(" · \"{p}\""))
                .unwrap_or_default()
        )?;
    }
    write!(
        out,
        "Resume which session? [1-{}, blank for most recent]: ",
        sessions.len()
    )?;
    out.flush()?;

    let mut line = String::new();
    input.read_line(&mut line)?;
    let trimmed = line.trim();
    let index = if trimmed.is_empty() {
        0
    } else {
        trimmed
            .parse::<usize>()
            .ok()
            .filter(|n| *n >= 1 && *n <= sessions.len())
            .map(|n| n - 1)
            .unwrap_or(0)
    };
    Ok(Some(sessions[index].clone()))
}

#[cfg(test)]
mod resume_cli_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_resume_flag() {
        let cli = Cli::parse_from(["local-code", "--resume"]);
        assert!(cli.resume);
    }

    #[test]
    fn resume_defaults_to_false() {
        let cli = Cli::parse_from(["local-code"]);
        assert!(!cli.resume);
    }
}

#[cfg(test)]
mod select_session_tests {
    use super::*;

    fn summary(connection: &str, updated_at: &str) -> SessionSummary {
        SessionSummary {
            path: format!("/sessions/{connection}.json").into(),
            connection_name: connection.into(),
            model_name: "m".into(),
            updated_at: updated_at.into(),
            first_user_turn_preview: None,
        }
    }

    #[test]
    fn returns_none_when_no_sessions_exist() {
        let mut out = Vec::new();
        let result = select_session_to_resume(&[], &b""[..], &mut out).unwrap();
        assert!(result.is_none());
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No previous sessions")
        );
    }

    #[test]
    fn auto_selects_the_only_session_without_prompting() {
        let sessions = vec![summary("only-one", "2026-07-06T00:00:00Z")];
        let mut out = Vec::new();
        let result = select_session_to_resume(&sessions, &b""[..], &mut out).unwrap();
        assert_eq!(result.unwrap().connection_name, "only-one");
    }

    #[test]
    fn blank_input_selects_the_most_recent() {
        let sessions = vec![
            summary("newest", "2026-07-06T00:00:00Z"),
            summary("older", "2026-07-01T00:00:00Z"),
        ];
        let mut out = Vec::new();
        let result = select_session_to_resume(&sessions, &b"\n"[..], &mut out).unwrap();
        assert_eq!(result.unwrap().connection_name, "newest");
    }

    #[test]
    fn numeric_input_selects_by_index() {
        let sessions = vec![
            summary("newest", "2026-07-06T00:00:00Z"),
            summary("older", "2026-07-01T00:00:00Z"),
        ];
        let mut out = Vec::new();
        let result = select_session_to_resume(&sessions, &b"2\n"[..], &mut out).unwrap();
        assert_eq!(result.unwrap().connection_name, "older");
    }

    #[test]
    fn out_of_range_input_falls_back_to_most_recent() {
        let sessions = vec![
            summary("newest", "2026-07-06T00:00:00Z"),
            summary("older", "2026-07-01T00:00:00Z"),
        ];
        let mut out = Vec::new();
        let result = select_session_to_resume(&sessions, &b"99\n"[..], &mut out).unwrap();
        assert_eq!(result.unwrap().connection_name, "newest");
    }
}

#[cfg(test)]
mod mcp_cli_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_mcp_list() {
        let cli = Cli::parse_from(["local-code", "mcp", "list"]);
        assert!(matches!(
            cli.command,
            Some(Command::Mcp {
                action: McpAction::List
            })
        ));
    }

    #[test]
    fn parses_mcp_remove_with_name() {
        let cli = Cli::parse_from(["local-code", "mcp", "remove", "fs"]);
        match cli.command {
            Some(Command::Mcp {
                action: McpAction::Remove { name },
            }) => assert_eq!(name, "fs"),
            other => panic!("expected Mcp::Remove, got {other:?}"),
        }
    }

    #[test]
    fn parses_mcp_add() {
        let cli = Cli::parse_from(["local-code", "mcp", "add"]);
        assert!(matches!(
            cli.command,
            Some(Command::Mcp {
                action: McpAction::Add
            })
        ));
    }
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

#[cfg(test)]
mod skills_cli_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_install_with_global_and_name() {
        let cli = Cli::parse_from([
            "local-code",
            "skills",
            "install",
            "owner/repo",
            "--global",
            "--name",
            "foo",
        ]);
        if let Some(Command::Skills { action }) = cli.command {
            if let SkillsAction::Install { spec, global, name } = action {
                assert_eq!(spec, "owner/repo");
                assert!(global);
                assert_eq!(name.as_deref(), Some("foo"));
            } else {
                panic!("expected SkillsAction::Install, got a different variant");
            }
        } else {
            panic!("expected Some(Command::Skills)");
        }
    }

    #[test]
    fn parses_list() {
        let cli = Cli::parse_from(["local-code", "skills", "list"]);
        assert!(matches!(
            cli.command,
            Some(Command::Skills {
                action: SkillsAction::List
            })
        ));
    }

    #[test]
    fn parses_remove_with_global() {
        let cli = Cli::parse_from(["local-code", "skills", "remove", "foo", "--global"]);
        if let Some(Command::Skills { action }) = cli.command {
            if let SkillsAction::Remove { name, global } = action {
                assert_eq!(name, "foo");
                assert!(global);
            } else {
                panic!("expected SkillsAction::Remove, got a different variant");
            }
        } else {
            panic!("expected Some(Command::Skills)");
        }
    }
}
