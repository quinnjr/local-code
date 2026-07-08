// src/agent/skill_tool.rs

use daimon::tool::{Tool, ToolOutput};

use crate::skills::types::Skill;

/// Exposes every `ModelInvoked` skill (see `crate::skills::types::LoadMode`)
/// as a single `skill` tool taking a skill `name`, returning that skill's
/// body. Constructed once at agent build time from the same discovered
/// `Skill` list used to build the auto-injected system-prompt context (see
/// `crate::skills::discovery::resolve_skill_context`) — mirrors
/// `crate::mcp::tool::NamespacedMcpTool` in holding pre-fetched state rather
/// than doing any I/O of its own at `execute` time.
pub struct SkillTool {
    skills: Vec<Skill>,
}

impl SkillTool {
    /// Filters `all_skills` down to just the model-invoked ones — callers
    /// don't need to pre-filter by `LoadMode` themselves.
    pub fn new(all_skills: Vec<Skill>) -> Self {
        let skills = all_skills
            .into_iter()
            .filter(|s| matches!(s.load_mode, crate::skills::types::LoadMode::ModelInvoked))
            .collect();
        Self { skills }
    }
}

impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Loads the full instructions for an installed skill by name. Use this when a skill \
         relevant to the current task was listed in your context."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The skill's name, as listed in context." }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, input: &serde_json::Value) -> daimon::Result<ToolOutput> {
        let Some(name) = input.get("name").and_then(|v| v.as_str()) else {
            return Ok(ToolOutput::error("missing required 'name' argument"));
        };
        match self.skills.iter().find(|s| s.name == name) {
            Some(skill) => Ok(ToolOutput::text(skill.body.clone())),
            None => Ok(ToolOutput::error(format!(
                "no skill named '{name}' is available. Available skills: {}",
                self.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::types::{LoadMode, Scope};
    use std::path::PathBuf;

    fn skill(name: &str, load_mode: LoadMode) -> Skill {
        Skill {
            name: name.to_string(),
            description: format!("{name} description"),
            scope: Scope::Project,
            dir: PathBuf::from("/unused"),
            body: format!("{name} body"),
            load_mode,
        }
    }

    #[tokio::test]
    async fn returns_the_body_of_a_known_model_invoked_skill() {
        let tool = SkillTool::new(vec![skill("pdf", LoadMode::ModelInvoked)]);
        let output = tool.execute(&serde_json::json!({"name": "pdf"})).await.unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "pdf body");
    }

    #[tokio::test]
    async fn errors_with_available_names_for_an_unknown_skill() {
        let tool = SkillTool::new(vec![skill("pdf", LoadMode::ModelInvoked)]);
        let output = tool.execute(&serde_json::json!({"name": "nope"})).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("pdf"));
    }

    #[tokio::test]
    async fn errors_when_name_argument_missing() {
        let tool = SkillTool::new(vec![skill("pdf", LoadMode::ModelInvoked)]);
        let output = tool.execute(&serde_json::json!({})).await.unwrap();
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn excludes_always_apply_and_globs_skills_from_the_lookup() {
        let tool = SkillTool::new(vec![
            skill("always-on", LoadMode::AlwaysApply),
            skill("conditional", LoadMode::Globs(vec!["*.pdf".to_string()])),
        ]);
        let output = tool.execute(&serde_json::json!({"name": "always-on"})).await.unwrap();
        assert!(output.is_error);
    }
}
