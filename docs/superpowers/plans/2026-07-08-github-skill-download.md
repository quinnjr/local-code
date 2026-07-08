# GitHub-Powered Skill Downloading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let local-code install, list, update, and remove "skills" (Claude-Code-style `SKILL.md`/Cursor-style `SKILL.mdc` directories) fetched directly from GitHub repos, and wire them into the agent so model-invoked skills are callable via a new `skill` tool while `alwaysApply`/glob-matched `.mdc` skills are auto-injected into the system prompt.

**Architecture:** A new `src/skills/` module owns everything skill-specific: GitHub spec parsing/fetching (`github.rs`), frontmatter parsing (`frontmatter.rs`), scope-aware discovery + glob-matching (`discovery.rs`), and install/update/remove/list (`install.rs`). A new `src/cli/skills.rs` exposes this over the CLI, following the existing `ConnectionsAction`/`MemoryAction` pattern. A new `agent/skill_tool.rs` exposes model-invoked skills as a built-in tool, following the existing `NamespacedMcpTool` pattern (a `Tool` impl holding pre-discovered state, not a `#[tool_fn]`). Discovered skills are threaded through the exact same call sites `NamespacedMcpTool`s already flow through (`register_all_tools`, `build_agent_with_mcp_tools`, `build_streaming_agent_with_history`, `rebuild_agent`, `AppProps`, `run_tui`, `run_headless`).

**Tech Stack:** Rust, `daimon` agent framework, `reqwest` (new dependency, GitHub API + raw file fetches), `wiremock` (new dev-dependency, mocked GitHub API in tests), existing `serde_json`, `glob`, `ignore`, `tempfile`.

---

## Spec coverage checklist (for self-review at the end)

- [ ] SKILL.md / SKILL.mdc frontmatter formats and precedence (.mdc wins if both present)
- [ ] Model-invoked vs alwaysApply vs glob-conditional load modes
- [ ] Glob matching against project tree at build time, respecting ignore rules
- [ ] GitHub Contents API fetch via `owner/repo[/path][@ref]` spec, default branch resolution, commit-SHA pinning
- [ ] Optional GitHub token via `SecretStore`
- [ ] Project vs global scope, project shadows global by name
- [ ] Install manifest (`.skill-manifest.json`) + atomic install
- [ ] CLI: install / list / remove / update
- [ ] `skill` tool (model-invoked skills only)
- [ ] Context injection (always-apply/glob-matched bodies + model-invoked listing)
- [ ] Error handling: network errors, malformed frontmatter (skip + warn, not fatal)
- [ ] Tests at every layer per the spec's Testing section

---

### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `reqwest` and `wiremock`**

Add to `[dependencies]` (after `ignore = "0.4"`):

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

Add to `[dev-dependencies]` (after `tokio = { version = "1", features = ["test-util"] }`):

```toml
wiremock = "0.6"
```

- [ ] **Step 2: Build to confirm the lockfile resolves and everything still compiles**

Run: `cargo build`
Expected: `Finished` with no errors (warnings from existing code are fine/pre-existing).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add reqwest and wiremock for GitHub skill downloading"
```

---

### Task 2: Skill core types

**Files:**
- Create: `src/skills/mod.rs`
- Create: `src/skills/types.rs`
- Modify: `src/lib.rs` (register the `skills` module)

- [ ] **Step 1: Check how modules are registered in `src/lib.rs`**

Run: `cat src/lib.rs`

You'll see a flat list of `pub mod X;` lines (e.g. `pub mod agent;`, `pub mod context;`, `pub mod mcp;`). Add `pub mod skills;` to that list, alphabetically placed.

- [ ] **Step 2: Create `src/skills/types.rs`**

```rust
// src/skills/types.rs

use std::path::PathBuf;

/// Which of the two config roots (`Paths::project_config_dir` /
/// `Paths::user_config_dir`) a skill was installed into. Project scope
/// shadows global scope for skills of the same name (see
/// `crate::skills::discovery::discover_skills`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
}

/// A parsed `owner/repo[/path][@ref]` skill source spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSource {
    pub owner: String,
    pub repo: String,
    /// Empty string means "repo root".
    pub path: String,
    /// `None` means "use the repo's default branch".
    pub git_ref: Option<String>,
}

/// How a discovered skill should be loaded into the agent. Determined by
/// `crate::skills::frontmatter::parse_frontmatter` from the file extension
/// (`SKILL.md` vs `SKILL.mdc`) and frontmatter fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadMode {
    /// Plain `SKILL.md`, or `.mdc` with no `globs` and no `alwaysApply: true`.
    /// Listed for the model; body loaded on demand via the `skill` tool.
    ModelInvoked,
    /// `.mdc` with `alwaysApply: true`. Always injected into the system
    /// prompt at agent build time.
    AlwaysApply,
    /// `.mdc` with a non-empty `globs` list (and not `alwaysApply: true`).
    /// Injected into the system prompt only if the project tree contains at
    /// least one matching file, evaluated once at agent build time.
    Globs(Vec<String>),
}

/// One discovered skill: parsed frontmatter plus body, plus where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub scope: Scope,
    /// The skill's directory, e.g. `.local-code/skills/pdf-extraction/`.
    pub dir: PathBuf,
    /// The skill body (frontmatter stripped).
    pub body: String,
    pub load_mode: LoadMode,
}

/// Sidecar manifest written alongside a skill's files at
/// `<skill_dir>/.skill-manifest.json`, recording where it came from so
/// `crate::skills::install::update_skill` can re-resolve and detect changes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledSkillManifest {
    pub owner: String,
    pub repo: String,
    pub path: String,
    /// The effective ref used at install time (either the user-supplied ref,
    /// or the repo's resolved default branch name).
    pub git_ref: String,
    /// The commit SHA that `git_ref` resolved to at install/update time.
    pub commit_sha: String,
}
```

- [ ] **Step 3: Create `src/skills/mod.rs`**

```rust
// src/skills/mod.rs

pub mod discovery;
pub mod frontmatter;
pub mod github;
pub mod install;
pub mod types;
```

(The `discovery`, `frontmatter`, `github`, and `install` submodules are created in later tasks; this file is written now with all four declared so `cargo build` doesn't need repeated edits. Create empty placeholder files for the three not yet created so the build succeeds after this task.)

- [ ] **Step 4: Create empty placeholder files for the not-yet-written submodules**

Run:
```bash
mkdir -p src/skills
touch src/skills/frontmatter.rs src/skills/github.rs src/skills/discovery.rs src/skills/install.rs
```

(These start as empty files — a `pub mod x;` pointing at an empty file compiles fine in Rust.)

- [ ] **Step 5: Build**

Run: `cargo build`
Expected: `Finished` with no errors.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/skills/
git commit -m "feat(skills): add core types (Scope, SkillSource, LoadMode, Skill, manifest)"
```

---

### Task 3: GitHub spec parsing

**Files:**
- Modify: `src/skills/github.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// src/skills/github.rs

use crate::skills::types::SkillSource;

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("invalid skill source '{0}': expected owner/repo[/path][@ref]")]
    InvalidSpec(String),
}

/// Parses an `owner/repo[/path][@ref]` skill source spec. The ref, if
/// present, is split off from the *last* `@` in the spec (GitHub owner/repo
/// names and paths cannot themselves contain `@`, so this is unambiguous in
/// practice). `path` is `""` when no subpath was given.
pub fn parse_spec(spec: &str) -> Result<SkillSource, GithubError> {
    let (rest, git_ref) = match spec.rsplit_once('@') {
        Some((rest, r)) if !r.is_empty() => (rest, Some(r.to_string())),
        _ => (spec, None),
    };

    let mut parts = rest.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty());
    let repo = parts.next().filter(|s| !s.is_empty());
    let path = parts.next().unwrap_or("").to_string();

    match (owner, repo) {
        (Some(owner), Some(repo)) => Ok(SkillSource {
            owner: owner.to_string(),
            repo: repo.to_string(),
            path,
            git_ref,
        }),
        _ => Err(GithubError::InvalidSpec(spec.to_string())),
    }
}

#[cfg(test)]
mod parse_spec_tests {
    use super::*;

    #[test]
    fn parses_owner_repo_only() {
        let source = parse_spec("anthropics/skills").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "");
        assert_eq!(source.git_ref, None);
    }

    #[test]
    fn parses_owner_repo_with_path() {
        let source = parse_spec("anthropics/skills/pdf").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "pdf");
        assert_eq!(source.git_ref, None);
    }

    #[test]
    fn parses_owner_repo_with_ref() {
        let source = parse_spec("anthropics/skills@main").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "");
        assert_eq!(source.git_ref, Some("main".to_string()));
    }

    #[test]
    fn parses_owner_repo_path_and_ref() {
        let source = parse_spec("anthropics/skills/pdf@v1.2.0").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "pdf");
        assert_eq!(source.git_ref, Some("v1.2.0".to_string()));
    }

    #[test]
    fn parses_nested_path() {
        let source = parse_spec("anthropics/skills/nested/pdf").unwrap();
        assert_eq!(source.path, "nested/pdf");
    }

    #[test]
    fn rejects_missing_repo() {
        let result = parse_spec("anthropics");
        assert!(matches!(result, Err(GithubError::InvalidSpec(_))));
    }

    #[test]
    fn rejects_empty_spec() {
        let result = parse_spec("");
        assert!(matches!(result, Err(GithubError::InvalidSpec(_))));
    }
}
```

- [ ] **Step 2: Run the tests to see them compile and pass (this is TDD-in-one-shot since the implementation is already above; run to confirm)**

Run: `cargo test --lib skills::github::parse_spec_tests`
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/skills/github.rs
git commit -m "feat(skills): parse owner/repo[/path][@ref] skill source specs"
```

---

### Task 4: GitHub client — fetching directories

**Files:**
- Modify: `src/skills/github.rs`

- [ ] **Step 1: Write the failing tests (using `wiremock` to fake the GitHub API)**

Append to `src/skills/github.rs`:

```rust
use std::path::PathBuf;

/// A minimal GitHub REST API client. `api_base` defaults to
/// `https://api.github.com` but is overridable so tests can point it at a
/// local `wiremock` server instead of the real network.
pub struct GithubClient {
    http: reqwest::Client,
    api_base: String,
    token: Option<String>,
}

/// One file fetched from a GitHub directory, with its path relative to the
/// directory that was fetched (not the repo root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedFile {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
}

#[derive(serde::Deserialize)]
struct RepoInfo {
    default_branch: String,
}

#[derive(serde::Deserialize)]
struct CommitInfo {
    sha: String,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ContentsResponse {
    Directory(Vec<ContentsEntry>),
    File(ContentsEntry),
}

#[derive(serde::Deserialize, Clone)]
struct ContentsEntry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    download_url: Option<String>,
}

impl GithubClient {
    pub fn new(token: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_base: "https://api.github.com".to_string(),
            token,
        }
    }

    #[cfg(test)]
    fn with_api_base(token: Option<String>, api_base: String) -> Self {
        Self { http: reqwest::Client::new(), api_base, token }
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.get(url).header("User-Agent", "local-code");
        if let Some(token) = &self.token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        req
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, GithubError> {
        let response = self.request(url).send().await.map_err(GithubError::Request)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GithubError::Api { status: status.as_u16(), url: url.to_string(), body });
        }
        response.json::<T>().await.map_err(GithubError::Request)
    }

    /// Resolves the repo's default branch name (used when the user didn't
    /// supply an explicit `@ref`).
    pub async fn resolve_default_branch(&self, owner: &str, repo: &str) -> Result<String, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}", self.api_base);
        let info: RepoInfo = self.get_json(&url).await?;
        Ok(info.default_branch)
    }

    /// Resolves a branch/tag/SHA `git_ref` to a concrete commit SHA, so the
    /// subsequent directory fetch is a stable snapshot even if the branch
    /// moves mid-fetch, and so `update_skill` has something to compare
    /// against later.
    pub async fn resolve_commit_sha(&self, owner: &str, repo: &str, git_ref: &str) -> Result<String, GithubError> {
        let url = format!("{}/repos/{owner}/{repo}/commits/{}", self.api_base, urlencoding_ref(git_ref));
        let info: CommitInfo = self.get_json(&url).await?;
        Ok(info.sha)
    }

    /// Recursively fetches every file under `path` (repo-relative) at
    /// `commit_sha`, returning each with a path relative to `path` itself.
    /// Errors with `GithubError::NotADirectory` if `path` points at a single
    /// file rather than a directory (skills must be installed from a
    /// directory per the design spec).
    pub async fn fetch_directory_files(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        commit_sha: &str,
    ) -> Result<Vec<FetchedFile>, GithubError> {
        let mut files = Vec::new();
        self.fetch_directory_files_into(owner, repo, path, path, commit_sha, &mut files).await?;
        Ok(files)
    }

    fn fetch_directory_files_into<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        base_path: &'a str,
        current_path: &'a str,
        commit_sha: &'a str,
        out: &'a mut Vec<FetchedFile>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), GithubError>> + Send + 'a>> {
        Box::pin(async move {
            let url = format!(
                "{}/repos/{owner}/{repo}/contents/{current_path}?ref={commit_sha}",
                self.api_base
            );
            let response: ContentsResponse = self.get_json(&url).await?;
            let entries = match response {
                ContentsResponse::Directory(entries) => entries,
                ContentsResponse::File(_) => {
                    return Err(GithubError::NotADirectory(current_path.to_string()));
                }
            };

            for entry in entries {
                match entry.entry_type.as_str() {
                    "dir" => {
                        self.fetch_directory_files_into(owner, repo, base_path, &entry.path, commit_sha, out).await?;
                    }
                    "file" => {
                        let download_url = entry.download_url.ok_or_else(|| GithubError::Api {
                            status: 0,
                            url: entry.path.clone(),
                            body: "file entry missing download_url".to_string(),
                        })?;
                        let response = self.request(&download_url).send().await.map_err(GithubError::Request)?;
                        let bytes = response.bytes().await.map_err(GithubError::Request)?.to_vec();
                        let relative = entry.path.strip_prefix(base_path).unwrap_or(&entry.path).trim_start_matches('/');
                        out.push(FetchedFile { relative_path: PathBuf::from(relative), bytes });
                    }
                    _ => {} // symlinks/submodules: skip, not relevant to skill content
                }
            }
            Ok(())
        })
    }
}

/// GitHub ref names can contain `/` (e.g. `feature/x`); percent-encode just
/// that character so the commits-endpoint path segment stays well-formed.
fn urlencoding_ref(git_ref: &str) -> String {
    git_ref.replace('/', "%2F")
}
```

Update the `GithubError` enum from Task 3 to add the new variants:

```rust
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("invalid skill source '{0}': expected owner/repo[/path][@ref]")]
    InvalidSpec(String),
    #[error("GitHub request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("GitHub API returned {status} for {url}: {body}")]
    Api { status: u16, url: String, body: String },
    #[error("'{0}' is a file, not a directory — skills must be installed from a directory")]
    NotADirectory(String),
}
```

Now add the wiremock-backed tests at the bottom of the file:

```rust
#[cfg(test)]
mod github_client_tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn resolves_default_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "default_branch": "main"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_api_base(None, server.uri());
        let branch = client.resolve_default_branch("acme", "widgets").await.unwrap();
        assert_eq!(branch, "main");
    }

    #[tokio::test]
    async fn resolves_commit_sha_for_a_ref() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sha": "abc123"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_api_base(None, server.uri());
        let sha = client.resolve_commit_sha("acme", "widgets", "main").await.unwrap();
        assert_eq!(sha, "abc123");
    }

    #[tokio::test]
    async fn sends_bearer_token_when_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "default_branch": "main"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_api_base(Some("test-token".to_string()), server.uri());
        let branch = client.resolve_default_branch("acme", "widgets").await.unwrap();
        assert_eq!(branch, "main");
    }

    #[tokio::test]
    async fn fetches_files_from_a_flat_directory() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf"))
            .and(query_param("ref", "abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "SKILL.md",
                    "path": "skills/pdf/SKILL.md",
                    "type": "file",
                    "download_url": format!("{}/raw/SKILL.md", server.uri())
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\n---\nbody"))
            .mount(&server)
            .await;

        let client = GithubClient::with_api_base(None, server.uri());
        let files = client.fetch_directory_files("acme", "widgets", "skills/pdf", "abc123").await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, PathBuf::from("SKILL.md"));
        assert_eq!(files[0].bytes, b"---\nname: pdf\n---\nbody");
    }

    #[tokio::test]
    async fn fetches_files_from_nested_subdirectories() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "SKILL.md",
                    "path": "skills/pdf/SKILL.md",
                    "type": "file",
                    "download_url": format!("{}/raw/SKILL.md", server.uri())
                },
                {
                    "name": "reference",
                    "path": "skills/pdf/reference",
                    "type": "dir",
                    "download_url": null
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf/reference"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "notes.md",
                    "path": "skills/pdf/reference/notes.md",
                    "type": "file",
                    "download_url": format!("{}/raw/notes.md", server.uri())
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("skill body")).mount(&server).await;
        Mock::given(method("GET")).and(path("/raw/notes.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("notes")).mount(&server).await;

        let client = GithubClient::with_api_base(None, server.uri());
        let mut files = client.fetch_directory_files("acme", "widgets", "skills/pdf", "abc123").await.unwrap();
        files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].relative_path, PathBuf::from("SKILL.md"));
        assert_eq!(files[1].relative_path, PathBuf::from("reference/notes.md"));
    }

    #[tokio::test]
    async fn errors_when_path_points_at_a_single_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "SKILL.md",
                "path": "skills/pdf/SKILL.md",
                "type": "file",
                "download_url": "https://example.invalid/SKILL.md"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::with_api_base(None, server.uri());
        let result = client.fetch_directory_files("acme", "widgets", "skills/pdf/SKILL.md", "abc123").await;
        assert!(matches!(result, Err(GithubError::NotADirectory(_))));
    }

    #[tokio::test]
    async fn surfaces_api_errors_with_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;

        let client = GithubClient::with_api_base(None, server.uri());
        let result = client.resolve_default_branch("acme", "widgets").await;
        assert!(matches!(result, Err(GithubError::Api { status: 404, .. })));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib skills::github`
Expected: all `github_client_tests` and `parse_spec_tests` pass (9 total).

- [ ] **Step 3: Commit**

```bash
git add src/skills/github.rs
git commit -m "feat(skills): GitHub client for resolving refs and fetching directories"
```

---

### Task 5: Frontmatter parsing

**Files:**
- Modify: `src/skills/frontmatter.rs`

- [ ] **Step 1: Write the implementation and tests**

```rust
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
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib skills::frontmatter`
Expected: 14 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/skills/frontmatter.rs
git commit -m "feat(skills): parse SKILL.md/.mdc frontmatter and classify load mode"
```

---

### Task 6: Discovery — scanning and scope shadowing

**Files:**
- Modify: `src/skills/discovery.rs`

- [ ] **Step 1: Write the implementation and tests**

```rust
// src/skills/discovery.rs

use std::collections::HashSet;
use std::path::Path;

use crate::config::paths::Paths;
use crate::skills::frontmatter::{classify, parse_frontmatter};
use crate::skills::types::{Scope, Skill};

/// Scans both scope directories (`<project_config_dir>/skills/`,
/// `<user_config_dir>/skills/`) for installed skills. Each immediate
/// subdirectory containing a `SKILL.mdc` or `SKILL.md` (`.mdc` wins if both
/// are present) is parsed into a `Skill`. Project-scope skills shadow
/// global-scope skills of the same name — a global skill is skipped
/// entirely if a project skill with the same name was already found.
/// Malformed skills (unparseable frontmatter) are skipped with a warning
/// printed to stderr rather than failing discovery for the rest.
pub fn discover_skills(paths: &Paths, project_root: &Path) -> Vec<Skill> {
    let _ = project_root; // reserved for glob-matching call sites (Task 7)
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut skills = Vec::new();

    for (dir, scope) in [
        (paths.project_config_dir.join("skills"), Scope::Project),
        (paths.user_config_dir.join("skills"), Scope::Global),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }
            match load_skill_dir(&skill_dir, scope) {
                Ok(skill) => {
                    if seen_names.contains(&skill.name) {
                        continue; // shadowed by a project-scope skill already found
                    }
                    seen_names.insert(skill.name.clone());
                    skills.push(skill);
                }
                Err(SkillLoadError::NoSkillFile) => {} // not a skill directory, ignore silently
                Err(SkillLoadError::Malformed(reason)) => {
                    eprintln!("warning: skipping skill at {}: {reason}", skill_dir.display());
                }
            }
        }
    }

    skills
}

enum SkillLoadError {
    NoSkillFile,
    Malformed(String),
}

fn load_skill_dir(dir: &Path, scope: Scope) -> Result<Skill, SkillLoadError> {
    let mdc_path = dir.join("SKILL.mdc");
    let md_path = dir.join("SKILL.md");
    let (path, is_mdc) = if mdc_path.is_file() {
        (mdc_path, true)
    } else if md_path.is_file() {
        (md_path, false)
    } else {
        return Err(SkillLoadError::NoSkillFile);
    };

    let content = std::fs::read_to_string(&path)
        .map_err(|e| SkillLoadError::Malformed(format!("failed to read {}: {e}", path.display())))?;
    let (frontmatter, body) = parse_frontmatter(&content)
        .map_err(|e| SkillLoadError::Malformed(e.to_string()))?;
    let load_mode = classify(&frontmatter, is_mdc);

    Ok(Skill {
        name: frontmatter.name,
        description: frontmatter.description,
        scope,
        dir: dir.to_path_buf(),
        body,
        load_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_paths(root: &Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    fn write_skill(dir: &Path, filename: &str, name: &str, description: &str, extra_frontmatter: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(filename),
            format!("---\nname: {name}\ndescription: {description}\n{extra_frontmatter}---\nbody for {name}"),
        )
        .unwrap();
    }

    #[test]
    fn discovers_no_skills_when_no_scope_dirs_exist() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let skills = discover_skills(&paths, root.path());
        assert!(skills.is_empty());
    }

    #[test]
    fn discovers_a_project_scope_skill() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        write_skill(&paths.project_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Extract PDFs", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf");
        assert_eq!(skills[0].description, "Extract PDFs");
        assert_eq!(skills[0].scope, Scope::Project);
        assert_eq!(skills[0].body.trim(), "body for pdf");
    }

    #[test]
    fn discovers_a_global_scope_skill() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        write_skill(&paths.user_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Extract PDFs", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].scope, Scope::Global);
    }

    #[test]
    fn project_scope_shadows_global_scope_by_name() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        write_skill(&paths.project_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Project version", "");
        write_skill(&paths.user_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Global version", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Project version");
        assert_eq!(skills[0].scope, Scope::Project);
    }

    #[test]
    fn mdc_takes_precedence_over_md_in_the_same_directory() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let dir = paths.project_config_dir.join("skills/pdf");
        write_skill(&dir, "SKILL.md", "pdf", "From md", "");
        write_skill(&dir, "SKILL.mdc", "pdf", "From mdc", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "From mdc");
    }

    #[test]
    fn malformed_skill_is_skipped_not_fatal() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/broken")).unwrap();
        std::fs::write(paths.project_config_dir.join("skills/broken/SKILL.md"), "no frontmatter here").unwrap();
        write_skill(&paths.project_config_dir.join("skills/ok"), "SKILL.md", "ok", "Fine", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "ok");
    }

    #[test]
    fn directories_without_a_skill_file_are_ignored() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/not-a-skill")).unwrap();
        std::fs::write(paths.project_config_dir.join("skills/not-a-skill/README.md"), "hi").unwrap();

        let skills = discover_skills(&paths, root.path());
        assert!(skills.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib skills::discovery`
Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/skills/discovery.rs
git commit -m "feat(skills): discover installed skills across project/global scope"
```

---

### Task 7: Discovery — glob matching and context injection

**Files:**
- Modify: `src/skills/discovery.rs`

- [ ] **Step 1: Write the implementation and tests**

Append to `src/skills/discovery.rs`:

```rust
use crate::skills::types::LoadMode;

/// The result of resolving which skills to auto-inject vs. list for the
/// model, computed once at agent build/rebuild time (never re-evaluated
/// per-turn — consistent with how `context::load_project_context` is
/// already loaded once per build).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillContext {
    /// `(name, body)` for every `AlwaysApply` skill and every `Globs` skill
    /// whose pattern matched at least one file in the project tree.
    pub injected: Vec<(String, String)>,
    /// `(name, description)` for every `ModelInvoked` skill (including a
    /// `Globs` skill whose pattern matched nothing — no: non-matching Globs
    /// skills are dropped entirely, see `resolve_skill_context`).
    pub listing: Vec<(String, String)>,
}

/// Classifies each discovered skill into `injected` or `listing`, matching
/// `Globs` skills against `project_root`'s file tree (respecting the same
/// ignore rules as the built-in `grep`/`glob` tools, via the `ignore` crate)
/// exactly once. A `Globs` skill whose pattern matches nothing in the tree
/// is dropped entirely — it is not auto-injected and not listed, since it
/// isn't relevant to this project.
pub fn resolve_skill_context(skills: &[Skill], project_root: &Path) -> SkillContext {
    let mut context = SkillContext::default();
    for skill in skills {
        match &skill.load_mode {
            LoadMode::AlwaysApply => context.injected.push((skill.name.clone(), skill.body.clone())),
            LoadMode::ModelInvoked => context.listing.push((skill.name.clone(), skill.description.clone())),
            LoadMode::Globs(globs) => {
                if project_tree_matches_any_glob(project_root, globs) {
                    context.injected.push((skill.name.clone(), skill.body.clone()));
                }
            }
        }
    }
    context
}

fn project_tree_matches_any_glob(project_root: &Path, globs: &[String]) -> bool {
    let patterns: Vec<glob::Pattern> = globs.iter().filter_map(|g| glob::Pattern::new(g).ok()).collect();
    if patterns.is_empty() {
        return false;
    }
    for entry in ignore::WalkBuilder::new(project_root).build().flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(file_name) = entry.file_name().to_str() else { continue };
        if patterns.iter().any(|p| p.matches(file_name)) {
            return true;
        }
    }
    false
}

/// Renders a `SkillContext` into the text appended to the system prompt:
/// the full bodies of injected skills, then a short listing directing the
/// model to the `skill` tool for the rest. Returns an empty string if there
/// is nothing to show (mirrors `context::load_project_context`'s behavior
/// for "no files found").
pub fn render_skill_context(context: &SkillContext) -> String {
    let mut sections = Vec::new();

    for (name, body) in &context.injected {
        sections.push(format!("## Skill: {name}\n\n{body}"));
    }

    if !context.listing.is_empty() {
        let mut listing = String::from(
            "## Available skills\n\nThe following skills are available via the `skill` tool. \
             Call `skill` with the skill's name to load its full instructions.\n\n",
        );
        for (name, description) in &context.listing {
            listing.push_str(&format!("- `{name}`: {description}\n"));
        }
        sections.push(listing);
    }

    sections.join("\n\n")
}
```

- [ ] **Step 2: Add tests**

Append inside the existing `#[cfg(test)] mod tests` block in `src/skills/discovery.rs`:

```rust
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

    #[test]
    fn always_apply_skill_is_injected() {
        let root = tempdir().unwrap();
        let skills = vec![skill("a", LoadMode::AlwaysApply)];
        let context = resolve_skill_context(&skills, root.path());
        assert_eq!(context.injected, vec![("a".to_string(), "a body".to_string())]);
        assert!(context.listing.is_empty());
    }

    #[test]
    fn model_invoked_skill_is_listed_not_injected() {
        let root = tempdir().unwrap();
        let skills = vec![skill("a", LoadMode::ModelInvoked)];
        let context = resolve_skill_context(&skills, root.path());
        assert!(context.injected.is_empty());
        assert_eq!(context.listing, vec![("a".to_string(), "a description".to_string())]);
    }

    #[test]
    fn globs_skill_is_injected_when_a_matching_file_exists() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("doc.pdf"), "").unwrap();
        let skills = vec![skill("pdf", LoadMode::Globs(vec!["*.pdf".to_string()]))];
        let context = resolve_skill_context(&skills, root.path());
        assert_eq!(context.injected, vec![("pdf".to_string(), "pdf body".to_string())]);
        assert!(context.listing.is_empty());
    }

    #[test]
    fn globs_skill_is_dropped_entirely_when_nothing_matches() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("doc.txt"), "").unwrap();
        let skills = vec![skill("pdf", LoadMode::Globs(vec!["*.pdf".to_string()]))];
        let context = resolve_skill_context(&skills, root.path());
        assert!(context.injected.is_empty());
        assert!(context.listing.is_empty());
    }

    #[test]
    fn globs_skill_matches_nested_files() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("nested/doc.pdf"), "").unwrap();
        let skills = vec![skill("pdf", LoadMode::Globs(vec!["*.pdf".to_string()]))];
        let context = resolve_skill_context(&skills, root.path());
        assert_eq!(context.injected.len(), 1);
    }

    #[test]
    fn render_skill_context_is_empty_when_nothing_to_show() {
        let rendered = render_skill_context(&SkillContext::default());
        assert!(rendered.is_empty());
    }

    #[test]
    fn render_skill_context_includes_injected_bodies_and_listing() {
        let context = SkillContext {
            injected: vec![("always-on".to_string(), "Always-on body".to_string())],
            listing: vec![("pdf".to_string(), "Extract PDFs".to_string())],
        };
        let rendered = render_skill_context(&context);
        assert!(rendered.contains("Always-on body"));
        assert!(rendered.contains("`pdf`: Extract PDFs"));
        assert!(rendered.contains("skill` tool"));
    }
```

Add `use std::path::PathBuf;` to the test module's imports if not already present via the outer `use super::*;`.

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib skills::discovery`
Expected: 13 tests pass (7 from Task 6 + 6 new).

- [ ] **Step 4: Commit**

```bash
git add src/skills/discovery.rs
git commit -m "feat(skills): glob-match and render auto-injected skill context"
```

---

### Task 8: Install — atomic install with manifest

**Files:**
- Modify: `src/skills/install.rs`

- [ ] **Step 1: Write the implementation and tests**

```rust
// src/skills/install.rs

use std::path::{Path, PathBuf};

use crate::config::paths::Paths;
use crate::skills::github::{FetchedFile, GithubClient, GithubError};
use crate::skills::types::{InstalledSkillManifest, Scope, SkillSource};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Github(#[from] GithubError),
    #[error("io error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("failed to (de)serialize skill manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("a skill named '{0}' is already installed in this scope; pass --name to choose a different name")]
    AlreadyInstalled(String),
    #[error("no skill named '{0}' is installed in this scope")]
    NotInstalled(String),
    #[error("fetched directory '{0}' contained no files")]
    EmptyDirectory(String),
}

fn skills_dir(paths: &Paths, scope: Scope) -> PathBuf {
    match scope {
        Scope::Project => paths.project_config_dir.join("skills"),
        Scope::Global => paths.user_config_dir.join("skills"),
    }
}

/// Derives the default install name from a source spec: the last path
/// segment if a subpath was given, otherwise the repo name.
pub fn default_name(source: &SkillSource) -> String {
    source
        .path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&source.repo)
        .to_string()
}

fn io_err(path: &Path, source: std::io::Error) -> InstallError {
    InstallError::Io { path: path.to_path_buf(), source }
}

/// Fetches `source` from GitHub and installs it as `name` into `scope`.
/// Fetches fully into a temp directory first, then renames it into place —
/// a failed install never leaves a partially-written skill directory behind.
/// Errors with `AlreadyInstalled` if a skill with this name already exists
/// in this scope (use `update_skill` to refresh an existing install).
pub async fn install_skill(
    client: &GithubClient,
    paths: &Paths,
    scope: Scope,
    source: &SkillSource,
    name: &str,
) -> Result<(), InstallError> {
    let target_dir = skills_dir(paths, scope).join(name);
    if target_dir.exists() {
        return Err(InstallError::AlreadyInstalled(name.to_string()));
    }

    let git_ref = match &source.git_ref {
        Some(r) => r.clone(),
        None => client.resolve_default_branch(&source.owner, &source.repo).await?,
    };
    let commit_sha = client.resolve_commit_sha(&source.owner, &source.repo, &git_ref).await?;
    let files = client.fetch_directory_files(&source.owner, &source.repo, &source.path, &commit_sha).await?;
    if files.is_empty() {
        return Err(InstallError::EmptyDirectory(source.path.clone()));
    }

    let manifest = InstalledSkillManifest {
        owner: source.owner.clone(),
        repo: source.repo.clone(),
        path: source.path.clone(),
        git_ref,
        commit_sha,
    };

    let parent = target_dir.parent().expect("skills dir always has a parent");
    std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    let temp_dir = parent.join(format!(".{name}.installing"));
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).map_err(|e| io_err(&temp_dir, e))?;
    }
    write_files(&temp_dir, &files, &manifest)?;

    std::fs::rename(&temp_dir, &target_dir).map_err(|e| io_err(&target_dir, e))?;
    Ok(())
}

fn write_files(dir: &Path, files: &[FetchedFile], manifest: &InstalledSkillManifest) -> Result<(), InstallError> {
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    for file in files {
        let dest = dir.join(&file.relative_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        std::fs::write(&dest, &file.bytes).map_err(|e| io_err(&dest, e))?;
    }
    let manifest_json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(dir.join(".skill-manifest.json"), manifest_json).map_err(|e| io_err(dir, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path as wpath};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_paths(root: &Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    async fn mock_server_with_one_file() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"default_branch": "main"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/pdf/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/SKILL.md", server.uri())}
            ])))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: d\n---\nbody"))
            .mount(&server).await;
        server
    }

    #[tokio::test]
    async fn installs_a_skill_into_project_scope() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = crate::skills::github::GithubClient::new(None);
        let client = client_with_base(client, &server);
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let skill_md = paths.project_config_dir.join("skills/pdf/SKILL.md");
        assert!(skill_md.exists());
        let manifest_path = paths.project_config_dir.join("skills/pdf/.skill-manifest.json");
        let manifest: InstalledSkillManifest = serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.commit_sha, "abc123");
        assert_eq!(manifest.git_ref, "main");
    }

    #[tokio::test]
    async fn refuses_to_overwrite_an_existing_install() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/pdf")).unwrap();
        let server = mock_server_with_one_file().await;
        let client = client_with_base(crate::skills::github::GithubClient::new(None), &server);
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(matches!(result, Err(InstallError::AlreadyInstalled(name)) if name == "pdf"));
    }

    #[test]
    fn default_name_uses_last_path_segment() {
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        assert_eq!(default_name(&source), "pdf");
    }

    #[test]
    fn default_name_falls_back_to_repo_when_no_path() {
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "".into(), git_ref: None };
        assert_eq!(default_name(&source), "widgets");
    }

    /// Test-only helper: rebuilds a `GithubClient` pointed at `server`'s URI.
    /// `GithubClient::with_api_base` is `#[cfg(test)]`-only and private to
    /// `skills::github`, so this helper lives here as a thin wrapper using
    /// the same constructor via a re-exported test hook.
    fn client_with_base(_unused: crate::skills::github::GithubClient, server: &MockServer) -> crate::skills::github::GithubClient {
        crate::skills::github::GithubClient::new_for_test(None, server.uri())
    }
}
```

- [ ] **Step 2: Expose a test-only constructor from `github.rs` for cross-module tests**

`GithubClient::with_api_base` from Task 4 is private and `#[cfg(test)]`-scoped to `skills::github`'s own test module, which cannot be called from `skills::install`'s tests. In `src/skills/github.rs`, change:

```rust
    #[cfg(test)]
    fn with_api_base(token: Option<String>, api_base: String) -> Self {
        Self { http: reqwest::Client::new(), api_base, token }
    }
```

to:

```rust
    /// Test-only: builds a client pointed at a fake API base (e.g. a
    /// `wiremock::MockServer`'s URI) instead of the real GitHub API.
    #[cfg(test)]
    pub fn new_for_test(token: Option<String>, api_base: String) -> Self {
        Self { http: reqwest::Client::new(), api_base, token }
    }
```

And update the two existing call sites inside `src/skills/github.rs`'s own tests (`GithubClient::with_api_base(...)` → `GithubClient::new_for_test(...)`) — there are 7 such call sites in `github_client_tests` from Task 4; replace all of them.

Run: `grep -rn "with_api_base" src/skills/github.rs` to confirm every call site is updated, then re-run `cargo test --lib skills::github` to confirm Task 4's tests still pass under the renamed method.

- [ ] **Step 3: Run the new tests**

Run: `cargo test --lib skills::install`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/skills/github.rs src/skills/install.rs
git commit -m "feat(skills): atomic install with manifest, expose test-only GithubClient ctor"
```

---

### Task 9: Install — update, remove, list

**Files:**
- Modify: `src/skills/install.rs`

- [ ] **Step 1: Write the implementation and tests**

Append to `src/skills/install.rs`:

```rust
/// Re-resolves `name`'s pinned ref to a commit SHA; if it has changed since
/// the manifest's recorded `commit_sha`, re-fetches and replaces the skill's
/// files (manifest included) atomically, the same way `install_skill` does.
/// No-op (returns `Ok(false)`) if the ref hasn't moved. Returns `Ok(true)` if
/// an update was applied.
pub async fn update_skill(client: &GithubClient, paths: &Paths, scope: Scope, name: &str) -> Result<bool, InstallError> {
    let dir = skills_dir(paths, scope).join(name);
    if !dir.exists() {
        return Err(InstallError::NotInstalled(name.to_string()));
    }
    let manifest_text = std::fs::read_to_string(dir.join(".skill-manifest.json")).map_err(|e| io_err(&dir, e))?;
    let manifest: InstalledSkillManifest = serde_json::from_str(&manifest_text)?;

    let latest_sha = client.resolve_commit_sha(&manifest.owner, &manifest.repo, &manifest.git_ref).await?;
    if latest_sha == manifest.commit_sha {
        return Ok(false);
    }

    let files = client.fetch_directory_files(&manifest.owner, &manifest.repo, &manifest.path, &latest_sha).await?;
    if files.is_empty() {
        return Err(InstallError::EmptyDirectory(manifest.path.clone()));
    }
    let new_manifest = InstalledSkillManifest { commit_sha: latest_sha, ..manifest };

    let parent = dir.parent().expect("skills dir always has a parent");
    let temp_dir = parent.join(format!(".{name}.installing"));
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).map_err(|e| io_err(&temp_dir, e))?;
    }
    write_files(&temp_dir, &files, &new_manifest)?;

    std::fs::remove_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
    std::fs::rename(&temp_dir, &dir).map_err(|e| io_err(&dir, e))?;
    Ok(true)
}

/// Removes an installed skill's directory entirely.
pub fn remove_skill(paths: &Paths, scope: Scope, name: &str) -> Result<(), InstallError> {
    let dir = skills_dir(paths, scope).join(name);
    if !dir.exists() {
        return Err(InstallError::NotInstalled(name.to_string()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| io_err(&dir, e))
}

/// One row of `local-code skills list` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledSkillSummary {
    pub name: String,
    pub scope: Scope,
    pub source: String, // "owner/repo/path@ref"
}

/// Lists every installed skill across both scopes (no shadowing applied here
/// — `list` shows everything installed, including a global skill that's
/// currently shadowed by a project skill of the same name, so the user can
/// see what `remove --global` would affect).
pub fn list_skills(paths: &Paths) -> Result<Vec<InstalledSkillSummary>, InstallError> {
    let mut summaries = Vec::new();
    for (dir, scope) in [
        (paths.project_config_dir.join("skills"), Scope::Project),
        (paths.user_config_dir.join("skills"), Scope::Global),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }
            let manifest_path = skill_dir.join(".skill-manifest.json");
            let Ok(manifest_text) = std::fs::read_to_string(&manifest_path) else { continue };
            let Ok(manifest) = serde_json::from_str::<InstalledSkillManifest>(&manifest_text) else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            let path_suffix = if manifest.path.is_empty() { String::new() } else { format!("/{}", manifest.path) };
            summaries.push(InstalledSkillSummary {
                name,
                scope,
                source: format!("{}/{}{}@{}", manifest.owner, manifest.repo, path_suffix, manifest.git_ref),
            });
        }
    }
    Ok(summaries)
}
```

- [ ] **Step 2: Add tests**

Append inside the existing `#[cfg(test)] mod tests` block in `src/skills/install.rs`:

```rust
    #[tokio::test]
    async fn update_is_a_noop_when_ref_has_not_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = client_with_base(crate::skills::github::GithubClient::new(None), &server);
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn update_refetches_when_ref_has_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = client_with_base(crate::skills::github::GithubClient::new(None), &server);
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        // Point the commit-resolution mock at a new sha and change the file content.
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "def456"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/pdf/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/SKILL2.md", server.uri())}
            ])))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/raw/SKILL2.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: updated\n---\nnew body"))
            .mount(&server).await;

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(updated);
        let content = std::fs::read_to_string(paths.project_config_dir.join("skills/pdf/SKILL.md")).unwrap();
        assert!(content.contains("updated"));
    }

    #[tokio::test]
    async fn update_errors_when_skill_not_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        let client = client_with_base(crate::skills::github::GithubClient::new(None), &server);
        let result = update_skill(&client, &paths, Scope::Project, "nope").await;
        assert!(matches!(result, Err(InstallError::NotInstalled(name)) if name == "nope"));
    }

    #[test]
    fn remove_deletes_the_skill_directory() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let dir = paths.project_config_dir.join("skills/pdf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "content").unwrap();

        remove_skill(&paths, Scope::Project, "pdf").unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn remove_errors_when_not_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let result = remove_skill(&paths, Scope::Project, "nope");
        assert!(matches!(result, Err(InstallError::NotInstalled(name)) if name == "nope"));
    }

    #[tokio::test]
    async fn list_reports_installed_skills_with_source_and_scope() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = client_with_base(crate::skills::github::GithubClient::new(None), &server);
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let summaries = list_skills(&paths).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "pdf");
        assert_eq!(summaries[0].scope, Scope::Project);
        assert_eq!(summaries[0].source, "acme/widgets/skills/pdf@main");
    }

    #[test]
    fn list_is_empty_when_nothing_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let summaries = list_skills(&paths).unwrap();
        assert!(summaries.is_empty());
    }
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib skills::install`
Expected: 11 tests pass (4 from Task 8 + 7 new).

- [ ] **Step 4: Commit**

```bash
git add src/skills/install.rs
git commit -m "feat(skills): update, remove, and list installed skills"
```

---

### Task 10: CLI surface

**Files:**
- Create: `src/cli/skills.rs`
- Modify: `src/cli/mod.rs`

- [ ] **Step 1: Write `src/cli/skills.rs`**

```rust
// src/cli/skills.rs

use std::io::Write;

use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use crate::skills::github::GithubClient;
use crate::skills::install::{default_name, install_skill, list_skills, remove_skill, update_skill};
use crate::skills::types::Scope;

fn github_client() -> anyhow::Result<GithubClient> {
    let token = SecretStore::get_api_key("github")?;
    Ok(GithubClient::new(token))
}

pub async fn install<W: Write>(
    paths: &Paths,
    spec: &str,
    global: bool,
    name_override: Option<&str>,
    mut out: W,
) -> anyhow::Result<()> {
    let source = crate::skills::github::parse_spec(spec)?;
    let name = name_override.map(str::to_string).unwrap_or_else(|| default_name(&source));
    let scope = if global { Scope::Global } else { Scope::Project };
    let client = github_client()?;

    install_skill(&client, paths, scope, &source, &name).await?;
    writeln!(out, "Installed skill '{name}' from {spec} ({})", scope_label(scope))?;
    Ok(())
}

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let summaries = list_skills(paths)?;
    if summaries.is_empty() {
        writeln!(out, "No skills installed.")?;
        return Ok(());
    }
    for summary in summaries {
        writeln!(out, "{} · {} · {}", summary.name, scope_label(summary.scope), summary.source)?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, global: bool, mut out: W) -> anyhow::Result<()> {
    let scope = if global { Scope::Global } else { Scope::Project };
    remove_skill(paths, scope, name)?;
    writeln!(out, "Removed skill '{name}' ({})", scope_label(scope))?;
    Ok(())
}

pub async fn update<W: Write>(paths: &Paths, name: Option<&str>, global: bool, mut out: W) -> anyhow::Result<()> {
    let scope = if global { Scope::Global } else { Scope::Project };
    let client = github_client()?;

    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => list_skills(paths)?.into_iter().filter(|s| s.scope == scope).map(|s| s.name).collect(),
    };

    if names.is_empty() {
        writeln!(out, "No skills installed in this scope.")?;
        return Ok(());
    }

    for name in names {
        let updated = update_skill(&client, paths, scope, &name).await?;
        if updated {
            writeln!(out, "Updated skill '{name}'")?;
        } else {
            writeln!(out, "Skill '{name}' is already up to date")?;
        }
    }
    Ok(())
}

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Project => "project",
        Scope::Global => "global",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        assert!(String::from_utf8(out).unwrap().contains("No skills installed"));
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
        assert!(String::from_utf8(out).unwrap().contains("Removed skill 'pdf'"));
    }
}
```

- [ ] **Step 2: Wire `SkillsAction` into `src/cli/mod.rs`**

Add `pub mod skills;` to the top of `src/cli/mod.rs` alongside the existing `pub mod connections;` / `pub mod memory;` lines.

Add a `Skills` variant to the `Command` enum (in `src/cli/mod.rs`):

```rust
    /// Manage skills (install/list/remove/update from GitHub)
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
```

Add a new `SkillsAction` enum below `MemoryAction`:

```rust
#[derive(Subcommand)]
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
```

Add dispatch arms in `run()`'s `match cli.command` block, alongside the existing `Command::Connections` / `Command::Memory` arms:

```rust
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
```

- [ ] **Step 3: Build and run the CLI arg-parsing tests**

Run: `cargo build && cargo test --lib cli::`
Expected: builds clean; existing `cli::` tests plus the 3 new `cli::skills::tests` pass.

- [ ] **Step 4: Manually verify `--help` renders correctly**

Run: `cargo run -- skills --help`
Expected: shows `install`, `list`, `remove`, `update` subcommands with the descriptions above.

- [ ] **Step 5: Commit**

```bash
git add src/cli/skills.rs src/cli/mod.rs
git commit -m "feat(cli): add 'skills install/list/remove/update' subcommands"
```

---

### Task 11: `skill` tool

**Files:**
- Create: `src/agent/skill_tool.rs`
- Modify: `src/agent/mod.rs` (register the new submodule)

- [ ] **Step 1: Check how `agent/mod.rs` registers submodules**

Run: `cat src/agent/mod.rs`

Add `pub mod skill_tool;` alongside the existing `pub mod tools;` / `pub mod gated_tool;` lines.

- [ ] **Step 2: Write `src/agent/skill_tool.rs`**

```rust
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
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --lib agent::skill_tool`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/agent/mod.rs src/agent/skill_tool.rs
git commit -m "feat(agent): add skill tool for loading model-invoked skills by name"
```

---

### Task 12: Register the skill tool in `register_all_tools`

**Files:**
- Modify: `src/agent/build.rs`

- [ ] **Step 1: Extend `register_all_tools` and `build_agent_with_mcp_tools` with a `skills` parameter**

In `src/agent/build.rs`, change the imports at the top to add:

```rust
use crate::agent::skill_tool::SkillTool;
use crate::skills::types::Skill;
```

Change `register_all_tools`'s signature and body:

```rust
pub fn register_all_tools(
    builder: AgentBuilder,
    gate: Arc<PermissionGate>,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
) -> AgentBuilder {
    let mut builder = builder
        .tool(GatedTool::new(ReadFile, gate.clone()))
        .tool(GatedTool::new(WriteFile, gate.clone()))
        .tool(GatedTool::new(EditFile, gate.clone()))
        .tool(GatedTool::new(Bash, gate.clone()))
        .tool(GatedTool::new(Grep, gate.clone()))
        .tool(GatedTool::new(Glob, gate.clone()))
        .tool(GatedTool::new(SkillTool::new(skills), gate.clone()));

    for tool in mcp_tools {
        builder = builder.tool(GatedTool::new(tool, gate.clone()));
    }

    builder
}
```

Change `build_agent_with_mcp_tools`'s signature and body:

```rust
pub fn build_agent_with_mcp_tools(
    model: SharedModel,
    gate: Arc<PermissionGate>,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
) -> daimon::Result<Agent> {
    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(DEFAULT_SYSTEM_PROMPT);
    register_all_tools(builder, gate, mcp_tools, skills).build()
}
```

Change `build_agent` to pass `Vec::new()` for the new parameter:

```rust
pub fn build_agent(model: SharedModel, gate: Arc<PermissionGate>) -> daimon::Result<Agent> {
    build_agent_with_mcp_tools(model, gate, Vec::new(), Vec::new())
}
```

- [ ] **Step 2: Fix the existing tests in `src/agent/build.rs` that call the now-3-arg-longer functions**

Run: `grep -n "register_all_tools(\|build_agent_with_mcp_tools(" src/agent/build.rs`

Every call site inside `#[cfg(test)] mod tests` that calls `register_all_tools(builder, gate, mcp_tools)` needs `, Vec::new()` appended (or `, some_skills_vec` where the test is specifically about tool registration count — check each call site's context before editing). Do the same for any `build_agent_with_mcp_tools(model, gate, tools)` call — append `, Vec::new()`.

- [ ] **Step 3: Build and test**

Run: `cargo build && cargo test --lib agent::build`
Expected: builds clean, all existing `agent::build` tests still pass with the new parameter threaded through.

- [ ] **Step 4: Commit**

```bash
git add src/agent/build.rs
git commit -m "feat(agent): thread discovered skills into register_all_tools"
```

---

### Task 13: Thread skills through the TUI (gated_tool, rebuild, app, mod)

**Files:**
- Modify: `src/tui/gated_tool.rs`
- Modify: `src/tui/rebuild.rs`
- Modify: `src/tui/app.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Extend `build_streaming_agent_with_history` in `src/tui/gated_tool.rs`**

Add `use crate::skills::types::Skill;` to the imports. Change the signature and body (find the existing `build_streaming_agent_with_history` function from the earlier `sed` output and extend it — the body after the `system_prompt` construction calls `register_all_tools(builder, gate, mcp_tools)`; update that call too):

```rust
pub fn build_streaming_agent_with_history(
    model: SharedModel,
    gate: Arc<PermissionGate>,
    initial_messages: Vec<Message>,
    extra_system_context: &str,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
) -> daimon::Result<Agent> {
    let system_prompt = if extra_system_context.trim().is_empty() {
        SYSTEM_PROMPT.to_string()
    } else {
        format!("{SYSTEM_PROMPT}\n\n{extra_system_context}")
    };
    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(system_prompt)
        .memory(std::sync::Arc::new(SeededMemory::new(initial_messages)));
    Ok(register_all_tools(builder, gate, mcp_tools, skills).build()?)
}
```

(Run `cat src/tui/gated_tool.rs` first to see the exact current body past what was shown earlier — e.g. the `.memory(...)` line and `.build()` call — and adapt this replacement to match the file's actual remaining lines rather than assuming; the signature change and the `register_all_tools` call's extra argument are the two things that must change.)

Also update `build_streaming_agent` (the simpler function above it) to pass `Vec::new()`:

```rust
pub fn build_streaming_agent(model: SharedModel, gate: Arc<PermissionGate>) -> daimon::Result<Agent> {
    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(SYSTEM_PROMPT);
    register_all_tools(builder, gate, Vec::new(), Vec::new()).build()
}
```

- [ ] **Step 2: Extend `rebuild_agent` in `src/tui/rebuild.rs`**

Add `use crate::skills::types::Skill;` to imports. Change the signature to add a `skills: Vec<Skill>` parameter (placed right after `mcp_tools`, mirroring its position in `build_streaming_agent_with_history`):

```rust
pub fn rebuild_agent(
    model: SharedModel,
    initial_tier: PermissionTier,
    always_allow: Vec<String>,
    always_deny: Vec<String>,
    initial_messages: Vec<Message>,
    extra_system_context: &str,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
    pending_permission: ntui::State<Option<PermissionRequest>>,
) -> (Arc<Agent>, Arc<PermissionGate>, ResponderHandle) {
    let prompter = NtuiPermissionPrompter::new(pending_permission);
    let responder = prompter.responder_handle();
    let settings = PermissionSettings { always_allow, always_deny };
    let gate = Arc::new(PermissionGate::new(initial_tier, settings, Arc::new(prompter)));
    let agent = Arc::new(
        build_streaming_agent_with_history(
            model,
            gate.clone(),
            initial_messages,
            extra_system_context,
            mcp_tools,
            skills,
        )
        .expect("agent construction should not fail"),
    );
    (agent, gate, responder)
}
```

Update the existing test in the same file (`rebuild_agent_produces_a_working_agent_seeded_with_history`'s `Harness` component) — its call to `rebuild_agent(...)` needs a `Vec::new()` argument inserted in the same position (right before `pending`).

- [ ] **Step 3: Run `src/tui/rebuild.rs`'s test to confirm it still passes**

Run: `cargo test --lib tui::rebuild`
Expected: 1 test passes.

- [ ] **Step 4: Thread `skills` through `AppProps` in `src/tui/app.rs`**

Add a field to `AppProps` right after `pub mcp_tools: Vec<crate::mcp::tool::NamespacedMcpTool>,`:

```rust
    /// Skills discovered once at `run_tui` startup (`skills::discovery::discover_skills`).
    /// Threaded through every agent rebuild exactly like `mcp_tools`, so
    /// `/model`/`/resume` never drop skills that were available at launch.
    pub skills: Vec<crate::skills::types::Skill>,
```

Add the matching line to `AppProps::default()`'s body, right after `mcp_tools: Vec::new(),`:

```rust
            skills: Vec::new(),
```

Run `grep -n "mcp_tools_snapshot\|mcp_tools" src/tui/app.rs` to find every place `mcp_tools`/`mcp_tools_snapshot` is read or cloned (there were 9 occurrences found earlier: lines around 205, 238, 248, 321, 371, 389, 476, plus the two declarations already handled). For each of those call sites — the mount-time snapshot, the `/model` handler, and the `/resume` handler — add a parallel `skills_snapshot`/`skills` clone and pass it into the corresponding `rebuild_agent(...)` call at the same argument position `mcp_tools` occupies (immediately before it, since Step 2 put `skills` right after `mcp_tools` in `rebuild_agent`'s signature). Concretely:

- Where you see `let mcp_tools_snapshot = props.mcp_tools.clone();`, add directly after it: `let skills_snapshot = props.skills.clone();`
- Where you see `let mcp_tools = props.mcp_tools.clone();` (mount effect), add directly after it: `let skills = props.skills.clone();`
- Where you see `mcp_tools_snapshot.clone()` used inside a closure before a `rebuild_agent` call, add `skills_snapshot.clone()` immediately after it in the same closure setup, and pass it into the `rebuild_agent(...)` call immediately after the `mcp_tools` argument.
- Where a `rebuild_agent(...)` call site passes `mcp_tools,` or `mcp_tools_snapshot.clone(),` as an argument, add `skills,` or `skills_snapshot.clone(),` as the very next argument (matching the new parameter order from Step 2).

- [ ] **Step 5: Wire the `App` component's `element!` macro invocations**

Run `grep -n "mcp_tools: props.mcp_tools" src/tui/app.rs` and `grep -n "element!" src/tui/app.rs` — wherever `App` is constructed via the `element!(App(... mcp_tools: ... ...))` macro syntax (mirroring how `src/tui/mod.rs`'s `ntui::render(ntui::element!(App(...)))` call passes every prop), add `skills: props.skills,` alongside the existing `mcp_tools: props.mcp_tools,` line, in every such invocation.

- [ ] **Step 6: Discover skills in `run_tui` (`src/tui/mod.rs`) and thread them into `AppProps`**

Add imports at the top of `src/tui/mod.rs`:

```rust
use crate::skills::discovery::{discover_skills, resolve_skill_context, render_skill_context};
```

In `run_tui`, right after the existing line:

```rust
    let system_context = load_project_context(paths, project_root);
```

add:

```rust
    let discovered_skills = discover_skills(paths, project_root);
    let skill_context = resolve_skill_context(&discovered_skills, project_root);
    let rendered_skill_context = render_skill_context(&skill_context);
    let system_context = if rendered_skill_context.is_empty() {
        system_context
    } else if system_context.is_empty() {
        rendered_skill_context
    } else {
        format!("{system_context}\n\n{rendered_skill_context}")
    };
```

(This shadows the original `let system_context = ...` binding with the skill-augmented version, so every existing downstream use of `system_context` — including the `AppProps { system_context, ... }` struct-init shorthand — picks it up automatically with no further edit needed there.)

Then, in the `AppProps { ... }` struct literal a few lines below (where `mcp_tools,` already appears via struct-init shorthand), add:

```rust
        skills: discovered_skills,
```

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: `Finished` with no errors. If there are lingering "missing field" or "wrong number of arguments" errors, they identify exactly which call site from Steps 4-6 was missed — fix each one following the same pattern (add a `skills`/`skills_snapshot` argument in the same position as the neighboring `mcp_tools`/`mcp_tools_snapshot` one) until it compiles clean.

- [ ] **Step 8: Run the full TUI test suite**

Run: `cargo test --lib tui::`
Expected: all existing `tui::` tests pass (the plumbing is additive — no existing test's assertions should need to change, only call-site argument lists).

- [ ] **Step 9: Commit**

```bash
git add src/tui/gated_tool.rs src/tui/rebuild.rs src/tui/app.rs src/tui/mod.rs
git commit -m "feat(tui): discover and thread skills through agent build/rebuild"
```

---

### Task 14: Thread skills through headless mode

**Files:**
- Modify: `src/agent/headless.rs`

- [ ] **Step 1: Discover skills in `run_headless` and pass them to `build_agent_with_mcp_tools`**

Add an import at the top of `src/agent/headless.rs`:

```rust
use crate::skills::discovery::discover_skills;
```

In `run_headless`, right before the existing line:

```rust
    let agent = build_agent_with_mcp_tools(model, gate, mcp_report.tools)?;
```

add:

```rust
    let skills = discover_skills(paths, _project_root);
```

and change that line to:

```rust
    let agent = build_agent_with_mcp_tools(model, gate, mcp_report.tools, skills)?;
```

Note: `run_headless`'s `_project_root: &Path` parameter is currently unused (underscore-prefixed) — per the design spec's known scoping decision, headless mode registers the `skill` tool (so `-p` prompts can still invoke a model-invoked skill by name if the model somehow knows to) but does **not** thread `extra_system_context`/auto-injected skill bodies through headless mode, consistent with the pre-existing TODO.md limitation #7 ("Headless mode doesn't load AGENTS.md/CLAUDE.md context"). Since `discover_skills` now uses `_project_root`, rename the parameter to `project_root` (drop the underscore) and update its one other reference in the function if any exists (check with `grep -n "_project_root" src/agent/headless.rs` first).

- [ ] **Step 2: Fix the test call site**

Run: `grep -n "build_agent_with_mcp_tools(" src/agent/headless.rs`

The test `mcp_report_errors_do_not_prevent_agent_construction` calls `build_agent_with_mcp_tools(model, gate, report.tools)` — append `, Vec::new()`.

- [ ] **Step 3: Build and test**

Run: `cargo build && cargo test --lib agent::headless`
Expected: builds clean, all existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/agent/headless.rs
git commit -m "feat(agent): discover and register skills in headless mode"
```

---

### Task 15: Full suite verification and TODO.md note

**Files:**
- Modify: `TODO.md`

- [ ] **Step 1: Run the entire test suite**

Run: `cargo test`
Expected: every test passes (including the pre-existing `ignored` live-server tests, which stay ignored). If anything fails, fix it before proceeding — do not skip this step.

- [ ] **Step 2: Add a known-limitation entry to `TODO.md`**

Open `TODO.md` and append a new numbered entry (following the existing numbering — check the current highest number first with `grep -n "^[0-9]*\." TODO.md | tail -1`) documenting the headless scoping decision from Task 14:

```markdown
N. **Headless mode registers the `skill` tool but doesn't auto-inject skill context.** Like
   limitation #7 (no AGENTS.md/CLAUDE.md context in headless), `-p` prompts get the `skill` tool
   for model-invoked skills but never see the auto-injected bodies of `alwaysApply`/glob-matched
   `.mdc` skills, since headless mode doesn't thread `extra_system_context` through at all.
   Deliberate scoping decision, not a bug — fixing it is the same follow-up as #7's.
```

- [ ] **Step 3: Commit**

```bash
git add TODO.md
git commit -m "docs: note headless skill-context limitation in TODO.md"
```

---

## Self-review notes

- Every spec-coverage checklist item above has a corresponding task: formats/precedence (Task 5/6), load modes (Task 5/7), glob matching (Task 7), GitHub fetching/ref resolution (Task 3/4), optional token (Task 10's `github_client()`), scope shadowing (Task 6), manifest/atomic install (Task 8/9), CLI (Task 10), `skill` tool (Task 11/12), context injection (Task 7/13), error handling (Tasks 3-9's error enums, discovery's skip-and-warn), and tests at every layer (every task has a Steps-run-tests step).
- Type/signature consistency was checked across tasks: `register_all_tools`'s 4th parameter (`skills: Vec<Skill>`, Task 12) matches `build_agent_with_mcp_tools`'s new 4th parameter (Task 12), `build_streaming_agent_with_history`'s new 6th parameter (Task 13), and `rebuild_agent`'s new 8th parameter (Task 13) — all named `skills: Vec<Skill>` in the same relative position (immediately after `mcp_tools`).
- No placeholders: every step shows complete code, not descriptions of code.
