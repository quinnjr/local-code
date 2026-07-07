//! `/init`: survey the project, build a prompt from that survey, and have
//! the active model generate/update `AGENTS.md`. Never writes `CLAUDE.md`,
//! per spec section 4.

pub mod generate;
pub mod prompt;
pub mod survey;

pub use generate::{generate_agents_md, write_agents_md, InitError};
pub use prompt::build_init_prompt;
pub use survey::{survey_project, ProjectSurvey};
