// src/skills/frontmatter.rs

use crate::skills::types::LoadMode;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrontmatterError {
    #[error("missing frontmatter: file must start with a '---' delimited block")]
    MissingFrontmatter,
    #[error("frontmatter is missing required field '{0}'")]
    MissingField(&'static str),
}

/// Parsed frontmatter fields, before `LoadMode` classification (which also
/// depends on whether the source file was `SKILL.md` or `SKILL.mdc` — see
/// `classify`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFrontmatter {
    pub name: String,
    pub description: String,
    pub globs: Vec<String>,
    pub always_apply: bool,
}

/// Splits `content` into (frontmatter block, body), then parses the
/// frontmatter block. Only supports the restricted schema this project
/// actually uses (`name`, `description` as bare/quoted scalar strings,
/// `alwaysApply` as `true`/`false`, `globs` as an inline `["a", "b"]` list) —
/// deliberately not a general YAML parser, since skill frontmatter never
/// needs more than this.
pub fn parse_frontmatter(content: &str) -> Result<(ParsedFrontmatter, String), FrontmatterError> {
    let content = content.strip_prefix("---\n").or_else(|| content.strip_prefix("---\r\n"))
        .ok_or(FrontmatterError::MissingFrontmatter)?;

    let end = content.find("\n---").ok_or(FrontmatterError::MissingFrontmatter)?;
    let block = &content[..end];
    let after_delim = &content[end + 4..];
    let body = after_delim.strip_prefix('\n').unwrap_or(after_delim).to_string();

    let mut name = None;
    let mut description = None;
    let mut globs = Vec::new();
    let mut always_apply = false;

    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => name = Some(unquote(value)),
            "description" => description = Some(unquote(value)),
            "alwaysApply" => always_apply = value == "true",
            "globs" => globs = parse_inline_string_array(value),
            _ => {} // unknown fields are ignored, not an error
        }
    }

    let name = name.ok_or(FrontmatterError::MissingField("name"))?;
    let description = description.ok_or(FrontmatterError::MissingField("description"))?;

    Ok((ParsedFrontmatter { name, description, globs, always_apply }, body))
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parses `["*.pdf", "*.docx"]`-style inline arrays of quoted strings.
/// Returns an empty vec for `[]` or anything that doesn't look like a
/// bracketed list.
fn parse_inline_string_array(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return Vec::new();
    };
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(unquote)
        .collect()
}

/// Classifies parsed frontmatter into a `LoadMode`, given whether the source
/// file was `.mdc` (globs/alwaysApply are only meaningful there — a plain
/// `SKILL.md` is always `ModelInvoked` regardless of any stray fields in its
/// frontmatter, per the design spec).
pub fn classify(frontmatter: &ParsedFrontmatter, is_mdc: bool) -> LoadMode {
    if !is_mdc {
        return LoadMode::ModelInvoked;
    }
    if frontmatter.always_apply {
        LoadMode::AlwaysApply
    } else if !frontmatter.globs.is_empty() {
        LoadMode::Globs(frontmatter.globs.clone())
    } else {
        LoadMode::ModelInvoked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_description() {
        let (fm, body) = parse_frontmatter("---\nname: pdf\ndescription: Extract PDFs\n---\nDo the thing.").unwrap();
        assert_eq!(fm.name, "pdf");
        assert_eq!(fm.description, "Extract PDFs");
        assert_eq!(body, "Do the thing.");
    }

    #[test]
    fn parses_quoted_values() {
        let (fm, _) = parse_frontmatter("---\nname: \"pdf\"\ndescription: 'Extract PDFs'\n---\nbody").unwrap();
        assert_eq!(fm.name, "pdf");
        assert_eq!(fm.description, "Extract PDFs");
    }

    #[test]
    fn parses_always_apply_true() {
        let (fm, _) = parse_frontmatter("---\nname: a\ndescription: b\nalwaysApply: true\n---\nbody").unwrap();
        assert!(fm.always_apply);
    }

    #[test]
    fn always_apply_defaults_to_false() {
        let (fm, _) = parse_frontmatter("---\nname: a\ndescription: b\n---\nbody").unwrap();
        assert!(!fm.always_apply);
    }

    #[test]
    fn parses_globs_inline_array() {
        let (fm, _) = parse_frontmatter("---\nname: a\ndescription: b\nglobs: [\"*.pdf\", \"*.docx\"]\n---\nbody").unwrap();
        assert_eq!(fm.globs, vec!["*.pdf".to_string(), "*.docx".to_string()]);
    }

    #[test]
    fn globs_defaults_to_empty() {
        let (fm, _) = parse_frontmatter("---\nname: a\ndescription: b\n---\nbody").unwrap();
        assert!(fm.globs.is_empty());
    }

    #[test]
    fn errors_when_frontmatter_delimiter_missing() {
        let result = parse_frontmatter("no frontmatter here");
        assert_eq!(result.unwrap_err(), FrontmatterError::MissingFrontmatter);
    }

    #[test]
    fn errors_when_name_missing() {
        let result = parse_frontmatter("---\ndescription: b\n---\nbody");
        assert_eq!(result.unwrap_err(), FrontmatterError::MissingField("name"));
    }

    #[test]
    fn errors_when_description_missing() {
        let result = parse_frontmatter("---\nname: a\n---\nbody");
        assert_eq!(result.unwrap_err(), FrontmatterError::MissingField("description"));
    }

    #[test]
    fn classify_plain_md_is_always_model_invoked() {
        let fm = ParsedFrontmatter { name: "a".into(), description: "b".into(), globs: vec!["*.pdf".into()], always_apply: true };
        assert_eq!(classify(&fm, false), LoadMode::ModelInvoked);
    }

    #[test]
    fn classify_mdc_always_apply() {
        let fm = ParsedFrontmatter { name: "a".into(), description: "b".into(), globs: vec![], always_apply: true };
        assert_eq!(classify(&fm, true), LoadMode::AlwaysApply);
    }

    #[test]
    fn classify_mdc_globs() {
        let fm = ParsedFrontmatter { name: "a".into(), description: "b".into(), globs: vec!["*.pdf".into()], always_apply: false };
        assert_eq!(classify(&fm, true), LoadMode::Globs(vec!["*.pdf".into()]));
    }

    #[test]
    fn classify_mdc_with_neither_is_model_invoked() {
        let fm = ParsedFrontmatter { name: "a".into(), description: "b".into(), globs: vec![], always_apply: false };
        assert_eq!(classify(&fm, true), LoadMode::ModelInvoked);
    }

    #[test]
    fn classify_always_apply_wins_over_globs() {
        let fm = ParsedFrontmatter { name: "a".into(), description: "b".into(), globs: vec!["*.pdf".into()], always_apply: true };
        assert_eq!(classify(&fm, true), LoadMode::AlwaysApply);
    }
}
