pub mod connections;

use crate::config::paths::Paths;
use clap::{Parser, Subcommand};
use std::io::{stdin, stdout};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "local-code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage LLM connections (add/list/remove)
    Connections {
        #[command(subcommand)]
        action: ConnectionsAction,
    },
}

#[derive(Subcommand)]
pub enum ConnectionsAction {
    Add,
    List,
    Remove { name: String },
}

pub fn run(cli: Cli, project_root: PathBuf) -> anyhow::Result<()> {
    let paths = Paths::resolve(&project_root)?;
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
        None => {
            println!("local-code: no command given. Try `local-code connections list`.");
        }
    }
    Ok(())
}
