//! `/init`: survey the project, build a prompt from that survey, and have
//! the active model generate/update `AGENTS.md`. Never writes `CLAUDE.md`,
//! per spec section 4.

pub mod generate;
pub mod prompt;
pub mod survey;

pub use generate::{InitError, generate_agents_md, write_agents_md};
pub use prompt::build_init_prompt;
pub use survey::{ProjectSurvey, survey_project};
