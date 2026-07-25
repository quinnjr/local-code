use std::io::Write;

use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use crate::skills::bitbucket::BitbucketClient;
use crate::skills::client::SkillClient;
use crate::skills::github::GithubClient;
use crate::skills::gitlab::GitlabClient;
use crate::skills::install::{
    default_name, install_skill, list_skills, remove_skill, skill_is_installed, update_skill,
};
use crate::skills::types::{Host, Scope};

pub(crate) fn skill_client(host: Host) -> anyhow::Result<SkillClient> {
    match host {
        Host::GitHub => Ok(SkillClient::GitHub(GithubClient::new(
            SecretStore::get_api_key("github")?,
        ))),
        Host::GitLab => Ok(SkillClient::GitLab(GitlabClient::new(
            SecretStore::get_api_key("gitlab")?,
        ))),
        Host::Bitbucket => {
            let credentials = match SecretStore::get_api_key("bitbucket")? {
                Some(combined) => {
                    let (user, pass) = combined.split_once(':').ok_or_else(|| {
                        anyhow::anyhow!(
                            "bitbucket credential is malformed: expected 'username:app_password', found no ':' separator"
                        )
                    })?;
                    Some((user.to_string(), pass.to_string()))
                }
                None => None,
            };
            Ok(SkillClient::Bitbucket(BitbucketClient::new(credentials)))
        }
    }
}

/// Completes a parsed spec's `SkillSource`, running the extra async
/// project-path resolution GitLab shorthand specs need (see
/// `skills::spec::parse_spec`). Shared by `skills install` and the
/// marketplace commands, which accept the same spec shapes.
pub(crate) async fn resolve_skill_source(
    client: &SkillClient,
    parsed: crate::skills::spec::ParsedSpec,
) -> anyhow::Result<crate::skills::types::SkillSource> {
    if !parsed.needs_project_path_resolution {
        return Ok(parsed.source);
    }
    let SkillClient::GitLab(gitlab_client) = client else {
        unreachable!("needs_project_path_resolution is only ever true for Host::GitLab specs")
    };
    let (project_path, in_repo_path) = gitlab_client
        .resolve_project_path(&parsed.source.repo)
        .await?;
    Ok(crate::skills::types::SkillSource {
        repo: project_path,
        path: in_repo_path,
        ..parsed.source
    })
}

/// Rejects a skill name that looks like it's trying to escape the skills
/// directory (path separators or a `..` segment), rather than silently
/// sanitizing it. Used at the CLI layer as defense-in-depth on top of the
/// `write_files` guard in `skills::install`.
pub(crate) fn validate_skill_name(name: &str) -> anyhow::Result<()> {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        anyhow::bail!("skill name '{name}' must not contain path separators or '..'");
    }
    Ok(())
}

pub async fn install<W: Write>(
    paths: &Paths,
    spec: &str,
    global: bool,
    name_override: Option<&str>,
    mut out: W,
) -> anyhow::Result<()> {
    let parsed = crate::skills::spec::parse_spec(spec)?;
    if let Some(n) = name_override {
        validate_skill_name(n)?;
    }
    let client = skill_client(parsed.source.host)?;
    let source = resolve_skill_source(&client, parsed).await?;

    let name = name_override
        .map(str::to_string)
        .unwrap_or_else(|| default_name(&source));
    validate_skill_name(&name)?;
    let scope = if global {
        Scope::Global
    } else {
        Scope::Project
    };

    install_skill(&client, paths, scope, &source, &name).await?;
    writeln!(
        out,
        "Installed skill '{name}' from {spec} ({})",
        scope_label(scope)
    )?;
    Ok(())
}

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let summaries = list_skills(paths)?;
    if summaries.is_empty() {
        writeln!(out, "No skills installed.")?;
        return Ok(());
    }
    for summary in summaries {
        writeln!(
            out,
            "{} · {} · {}",
            summary.name,
            scope_label(summary.scope),
            summary.source
        )?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, global: bool, mut out: W) -> anyhow::Result<()> {
    validate_skill_name(name)?;
    let scope = if global {
        Scope::Global
    } else {
        Scope::Project
    };
    remove_skill(paths, scope, name)?;
    writeln!(out, "Removed skill '{name}' ({})", scope_label(scope))?;
    Ok(())
}

pub async fn update<W: Write>(
    paths: &Paths,
    name: Option<&str>,
    global: bool,
    mut out: W,
) -> anyhow::Result<()> {
    if let Some(n) = name {
        validate_skill_name(n)?;
    }
    let scope = if global {
        Scope::Global
    } else {
        Scope::Project
    };

    // An explicitly named skill must actually be installed: silently
    // "updating" nothing would exit 0, leaving scripts no way to tell the
    // skill was missing (the filter loop below would just drop the name).
    // Fail with the same `NotInstalled` contract `update_skill` reports.
    if let Some(n) = name
        && !skill_is_installed(paths, scope, n)
    {
        return Err(crate::skills::install::InstallError::NotInstalled(n.to_string()).into());
    }

    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => list_skills(paths)?
            .into_iter()
            .filter(|s| s.scope == scope)
            .map(|s| s.name)
            .collect(),
    };

    if names.is_empty() {
        writeln!(out, "No skills installed in this scope.")?;
        return Ok(());
    }

    // Plugin-installed skills are managed through their marketplace: this
    // command leaves them to `plugin update`, which re-fetches through the
    // marketplace catalog rather than the manifest's own coordinates (the
    // only coordinates that exist for local-marketplace installs). Each
    // updatable skill's manifest is retained here so the host resolution
    // below doesn't have to read it a second time.
    let named = name.is_some();
    let mut updatable = Vec::with_capacity(names.len());
    for name in names {
        let manifest = match crate::skills::install::read_skill_manifest(paths, scope, &name) {
            Ok(m) => m,
            Err(e) => {
                // Warn-and-continue is only for the all-in-scope enumeration,
                // where one broken skill shouldn't block the rest; an
                // explicitly named skill that can't be read is a hard error.
                if named {
                    return Err(e.into());
                }
                writeln!(out, "warning: skipping skill '{name}': {e}")?;
                continue;
            }
        };
        if let Some(plugin) = &manifest.plugin {
            writeln!(
                out,
                "Skill '{name}' is managed by plugin '{}@{}' — use `local-code plugin update {}` to update it",
                plugin.plugin, plugin.marketplace, plugin.plugin
            )?;
        } else {
            updatable.push((name, manifest));
        }
    }

    // Resolve every skill's host and build one client per distinct host up
    // front, so the concurrent updates below share a connection pool per host
    // instead of opening a fresh client (new TLS session) per skill.
    let mut names = Vec::with_capacity(updatable.len());
    let mut hosts = Vec::with_capacity(updatable.len());
    let mut clients: Vec<(Host, SkillClient)> = Vec::new();
    for (name, manifest) in updatable {
        let host = manifest.host;
        if !clients.iter().any(|(h, _)| *h == host) {
            clients.push((host, skill_client(host)?));
        }
        names.push(name);
        hosts.push(host);
    }

    // Each skill's update is an independent network round-trip, so run them
    // concurrently and report in the original list order once all complete.
    // Unlike the old serial loop, one skill's failure no longer prevents the
    // rest from updating; failures are reported per skill plus a summary error.
    let results = futures::future::join_all(names.iter().zip(&hosts).map(|(name, host)| {
        let client = &clients
            .iter()
            .find(|(h, _)| h == host)
            .expect("client built for every resolved host above")
            .1;
        async move { update_skill(client, paths, scope, name).await }
    }))
    .await;

    let mut failures = 0usize;
    for (name, result) in names.iter().zip(results) {
        match result {
            Ok(true) => writeln!(out, "Updated skill '{name}'")?,
            Ok(false) => writeln!(out, "Skill '{name}' is already up to date")?,
            Err(e) => {
                failures += 1;
                writeln!(out, "Failed to update skill '{name}': {e}")?;
            }
        }
    }
    if failures > 0 {
        anyhow::bail!("{failures} skill update(s) failed");
    }
    Ok(())
}

pub(crate) fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Project => "project",
        Scope::Global => "global",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::types::SkillSource;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path as wpath};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_paths(root: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    #[test]
    fn list_reports_no_skills_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No skills installed")
        );
    }

    #[test]
    fn remove_reports_not_installed_error() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let out: Vec<u8> = Vec::new();
        let result = remove(&paths, "nope", false, out);
        assert!(result.is_err());
    }

    #[test]
    fn remove_reports_success() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/pdf")).unwrap();
        let mut out = Vec::new();
        remove(&paths, "pdf", false, &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Removed skill 'pdf'")
        );
    }

    #[test]
    fn validate_skill_name_rejects_traversal_shapes() {
        assert!(validate_skill_name("../escape").is_err());
        assert!(validate_skill_name("foo/../bar").is_err());
        assert!(validate_skill_name("foo/bar").is_err());
        assert!(validate_skill_name("foo\\bar").is_err());
        assert!(validate_skill_name("pdf").is_ok());
    }

    #[test]
    fn remove_rejects_a_path_traversal_shaped_name() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        // Create a real skill outside the (nonexistent) `project/.local-code`
        // root, at the location `../escape` would resolve to if traversal
        // weren't blocked, so a would-be escape has something to hit.
        let escape_target = root.path().join("escape");
        std::fs::create_dir_all(&escape_target).unwrap();

        let out: Vec<u8> = Vec::new();
        let result = remove(&paths, "../escape", false, out);
        assert!(result.is_err());
        // The directory a successful traversal would have deleted must still exist.
        assert!(escape_target.exists());
    }

    #[tokio::test]
    async fn install_rejects_a_path_traversal_shaped_name_override() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let out: Vec<u8> = Vec::new();
        // An explicit `--name` override is validated eagerly, right after spec
        // parsing and before any host client is constructed or any network call
        // is made, so this fails fast on the bad `--name` regardless of `spec`
        // validity.
        let result = install(&paths, "acme/widgets", false, Some("../escape"), out).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must not contain path separators")
        );
    }

    #[tokio::test]
    async fn update_rejects_a_path_traversal_shaped_name() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let out: Vec<u8> = Vec::new();
        let result = update(&paths, Some("../escape"), false, out).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must not contain path separators")
        );
    }

    #[tokio::test]
    async fn update_a_single_missing_skill_errors_instead_of_exiting_ok() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let out: Vec<u8> = Vec::new();
        // Previously this dropped the name with a stderr warning and exited
        // `Ok(())` with no output, so scripts couldn't detect the failure.
        let result = update(&paths, Some("missing"), false, out).await;
        let err = result.expect_err("updating a skill that isn't installed must fail");
        assert_eq!(
            err.to_string(),
            "no skill named 'missing' is installed in this scope"
        );
    }

    #[tokio::test]
    async fn update_a_single_skill_with_an_unreadable_manifest_errors() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        // The skill directory exists (so the up-front installed check
        // passes) but there's no manifest to read: warn-and-skip is only
        // for the all-in-scope enumeration, not an explicitly named skill.
        std::fs::create_dir_all(paths.project_config_dir.join("skills/broken")).unwrap();
        let mut out = Vec::new();
        let result = update(&paths, Some("broken"), false, &mut out).await;
        assert!(
            result.is_err(),
            "expected a hard error, got ok with output: {}",
            String::from_utf8_lossy(&out)
        );
    }

    // `update()`'s `None`-name branch (list every skill in scope, call
    // `update_skill` on each, print a per-skill status line) can't be driven
    // through the public `update()` fn itself in a test: it builds its own
    // `SkillClient` via `skill_client()`, which always points at the real
    // hosted APIs with no injection point (unlike `GithubClient::new_for_test`,
    // there's no override reachable from here without touching production
    // code, which this fix is not allowed to do). So this test drives the
    // exact same loop body — list the scope's skills, call `update_skill` on
    // each, format the same two messages — against a mocked `SkillClient`,
    // which is where dependency injection is actually available. This
    // exercises the same untested branching (moved vs. not-moved, per skill,
    // across multiple skills) that the production loop contains.
    async fn update_all_in_scope_with_client<W: Write>(
        client: &SkillClient,
        paths: &Paths,
        scope: Scope,
        mut out: W,
    ) -> anyhow::Result<()> {
        let names: Vec<String> = list_skills(paths)?
            .into_iter()
            .filter(|s| s.scope == scope)
            .map(|s| s.name)
            .collect();
        for name in names {
            let updated = update_skill(client, paths, scope, &name).await?;
            if updated {
                writeln!(out, "Updated skill '{name}'")?;
            } else {
                writeln!(out, "Skill '{name}' is already up to date")?;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn update_all_in_scope_updates_and_reports_each_skill() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;

        // Two skills from two different repos, so their commit-resolution
        // endpoints can be moved independently of one another.
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/widgets"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "w1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/widgets/contents/skills/alpha"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/alpha/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/alpha.md", server.uri())}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/alpha.md"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("---\nname: alpha\ndescription: d\n---\nbody"),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/commits/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "g1"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/contents/skills/beta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/beta/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/beta.md", server.uri())}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/beta.md"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("---\nname: beta\ndescription: d\n---\nbody"),
            )
            .mount(&server)
            .await;

        let client = SkillClient::GitHub(GithubClient::new_for_test(None, server.uri()));
        let alpha_source = SkillSource {
            host: Host::GitHub,
            owner: "acme".into(),
            repo: "widgets".into(),
            path: "skills/alpha".into(),
            git_ref: None,
        };
        let beta_source = SkillSource {
            host: Host::GitHub,
            owner: "acme".into(),
            repo: "gadgets".into(),
            path: "skills/beta".into(),
            git_ref: None,
        };
        install_skill(&client, &paths, Scope::Project, &alpha_source, "alpha")
            .await
            .unwrap();
        install_skill(&client, &paths, Scope::Project, &beta_source, "beta")
            .await
            .unwrap();

        // Move `widgets`' ref (and its file content) to a new commit; leave
        // `gadgets` untouched so `beta` reports as already up to date.
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "w2"})),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/widgets/contents/skills/alpha"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/alpha/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/alpha2.md", server.uri())}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/alpha2.md"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("---\nname: alpha\ndescription: updated\n---\nnew body"),
            )
            .with_priority(1)
            .mount(&server)
            .await;

        let mut out = Vec::new();
        update_all_in_scope_with_client(&client, &paths, Scope::Project, &mut out)
            .await
            .unwrap();

        let output = String::from_utf8(out).unwrap();
        assert!(
            output.contains("Updated skill 'alpha'"),
            "missing moved-skill line, got: {output}"
        );
        assert!(
            output.contains("Skill 'beta' is already up to date"),
            "missing not-moved-skill line, got: {output}"
        );
    }

    #[test]
    fn skill_client_maps_each_host_to_its_concrete_client_variant() {
        assert!(matches!(
            skill_client(Host::GitHub).unwrap(),
            SkillClient::GitHub(_)
        ));
        assert!(matches!(
            skill_client(Host::GitLab).unwrap(),
            SkillClient::GitLab(_)
        ));
        assert!(matches!(
            skill_client(Host::Bitbucket).unwrap(),
            SkillClient::Bitbucket(_)
        ));
    }

    #[tokio::test]
    async fn update_skips_plugin_managed_skills_without_touching_the_network() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let skill_dir = paths.project_config_dir.join("skills/bar");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "body").unwrap();
        let manifest = crate::skills::types::InstalledSkillManifest {
            host: Host::GitHub,
            owner: "acme".into(),
            repo: "plugins".into(),
            path: "plugins/foo/skills/bar".into(),
            git_ref: "main".into(),
            commit_sha: "mkt1".into(),
            plugin: Some(crate::skills::types::PluginProvenance {
                marketplace: "acme-tools".into(),
                plugin: "foo".into(),
            }),
        };
        std::fs::write(
            skill_dir.join(".skill-manifest.json"),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();

        let mut out = Vec::new();
        // No mock server: the skip must happen before any client is built
        // or any request is made.
        update(&paths, Some("bar"), false, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("managed by plugin 'foo@acme-tools'"),
            "got: {text}"
        );
    }
}
