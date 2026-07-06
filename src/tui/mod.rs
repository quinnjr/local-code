//! The interactive `ntui` TUI shell. `run_tui` is the entry point the CLI calls
//! when invoked with no `-p` flag; everything else in this module supports it.

pub mod app;
pub mod components;
pub mod gated_tool;
pub mod permission_prompter;
pub mod state;

pub use app::{App, AppProps};
