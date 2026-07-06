# Core Agent Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the ReAct agent loop on top of `daimon::Agent`: construct a `Model` from a
`local_code::config::connection::Connection`, register the six built-in tools, enforce tiered
permissions (ask / auto-accept-edits / full-auto) around every write/edit/bash call, and expose it
all through a headless `local-code -p "<prompt>"` CLI mode. No TUI, no slash commands, no MCP, no
session persistence — those are later phases.

**Architecture:** `local_code::agent::provider::build_model` turns a `Connection` +
`Option<String>` API key into a `daimon::model::SharedModel` (`OpenAi::with_base_url(...)` for
`ProviderKind::OpenAiCompatible`, `Ollama::with_base_url(...)` for `ProviderKind::Ollama`). Six
`#[tool_fn]`-annotated async functions (`read_file`, `write_file`, `edit_file`, `bash`, `grep`,
`glob`) are registered into a `daimon::tool::ToolRegistry`-backed `AgentBuilder`. Because
`#[tool_fn]` generates stateless unit structs, permission enforcement cannot live inside the tool
bodies as originally written by the macro — but it also must not depend on
`daimon::middleware::Middleware` being invoked, because (as later verified in Phase 3 by reading the
real vendored `daimon-0.16.0` source, `src/agent/runner.rs`) `Agent::prompt_stream` calls
`tool.execute_erased(...)` directly and never runs the middleware stack, while `Agent::prompt` does.
Relying on `Middleware` would therefore silently stop enforcing permissions the moment any caller
(the TUI, Phase 3) drives the agent via `prompt_stream` instead of `prompt`. This plan instead
defines the canonical `local_code::agent::gated_tool::GatedTool<T>` — a `daimon::tool::Tool`
wrapper that consults a `local_code::permissions::PermissionGate` (tier + project allow/deny list +
pluggable `PermissionPrompter`) *inside its own `execute()`* before delegating to the wrapped tool.
Because both `Agent::prompt` and `Agent::prompt_stream` call `execute()` (or `execute_erased`,
which dispatches to the same `execute`) unconditionally, gating works identically under streaming
and non-streaming — there is exactly one enforcement mechanism, used everywhere, and no
`daimon::middleware::Middleware` is used anywhere in this plan. The permission *decision* logic
(`PermissionGate::check`, `PermissionPrompter` trait) is fully decoupled from how a prompt is
rendered — a `StdioPrompter` renders it over plain stdin/stdout for this phase; the TUI phase later
provides an `ntui`-rendering implementation of the same trait with zero changes to `PermissionGate`,
and reuses this exact `GatedTool<T>` (imported, not redefined) for its own streaming agent
construction. `local_code::agent::build::register_all_tools` is the single function that registers
every available tool (each wrapped in `GatedTool`) onto an `AgentBuilder`; `build_agent` calls it to
produce a `daimon::agent::Agent` for the headless path. `register_all_tools` in this phase only
knows about the six built-ins — Phase 5 (MCP client) later extends its signature in place to also
register MCP-discovered tools (see this plan's Self-review notes for the explicit follow-up), and
Phase 4 (TUI rebuild) calls the very same function so the TUI never drifts out of sync with the
headless path. `local_code::agent::headless::run_headless` drives one `agent.prompt()` call to
completion for the `-p` CLI flag, defaulting the permission tier to `full-auto` (overridable via
`--permission-mode`).

**Tech Stack:** Rust 2024 edition, `daimon` 0.16.0 (features `openai`, `ollama`, `macros`), `tokio`
1.x (`full`), `regex`, `walkdir`, `glob`, `serde_json`, `clap` (derive), `tempfile` (dev-dependency).
Builds directly on Phase 1's `local_code::config::{paths::Paths, connection::{Connection,
ProviderKind, ConnectionsFile, load_connections}, secrets::SecretStore}`.

---

## Spec traceability

This plan implements spec sections 2 ("Agent loop"), 3 ("Permissions"), and the headless half of
section 8 ("Invocation modes") from
`docs/superpowers/specs/2026-07-06-local-code-tui-design.md`. It deliberately does **not**
implement: the TUI (section 5, Phase 3), slash commands (section 6, Phase 3), session persistence
/ `--resume` (section 7, Phase 4), MCP tool wiring (Phase 5), or AGENTS.md/CLAUDE.md context
loading (section 4, folded into a later phase) — headless mode here uses a minimal hardcoded
system prompt.

Types/functions later phases will depend on (must be imported verbatim, not redefined):

- `local_code::permissions::types::{PermissionTier, PermissionDecision, PermissionRequest, PermissionPrompter, ToolKind, classify_tool}`
  — the TUI phase implements its own `PermissionPrompter` (rendering prompts as inline
  collapsible cards instead of stdio) and passes it into `PermissionGate::new` unchanged.
- `local_code::permissions::gate::{PermissionGate, CheckOutcome}` — the decision engine; the TUI
  reuses this exactly, only swapping the prompter.
- `local_code::permissions::settings::{PermissionSettings, SettingsFile, load_settings}` — the
  `/permissions` slash command (Phase 3) will read/write through this.
- `local_code::agent::provider::{build_model, ProviderError}` — the `/model` slash command
  (Phase 3) will call this when switching connections mid-session.
- `local_code::agent::gated_tool::GatedTool` — the *only* permission-enforcement wrapper in the
  whole project. Defined once, here, and imported verbatim by Phase 3 (its own
  `build_streaming_agent`), Phase 4 (`build_streaming_agent_with_history`), and Phase 5 (MCP tools
  are wrapped in `GatedTool<NamespacedMcpTool>` before registration) — never redefined.
- `local_code::agent::build::register_all_tools` — registers every available tool (each already
  wrapped in `GatedTool`) onto an `AgentBuilder`. This phase's version only knows about the six
  built-ins; Phase 5 extends its signature in place to add MCP-discovered tools, and both the
  headless path (`build_agent`, this phase) and the TUI path (Phase 4's
  `build_streaming_agent_with_history`/`rebuild_agent`) call this one function so they can never
  register a different tool set from each other.
- `local_code::agent::build::build_agent` — constructs the `daimon::agent::Agent` for headless use
  via `register_all_tools`. The TUI phase builds its own streaming-oriented sibling
  (`build_streaming_agent`, later `build_streaming_agent_with_history`) that also calls
  `register_all_tools`, rather than reusing `build_agent` itself (which hardcodes a
  non-conversation-preserving `AgentBuilder::new()` construction) — but both ultimately register the
  identical, `GatedTool`-wrapped tool set.
- `local_code::agent::headless::run_headless` — called directly by the CLI's `-p` flag; the
  session-persistence phase will wrap it to also serialize the resulting `AgentResponse.messages`.

---

## File structure

- Modify: `Cargo.toml` — add `daimon`, `tokio`, `regex`, `walkdir`, `glob`, `serde_json`
- Modify: `src/lib.rs` — add `pub mod permissions; pub mod agent;`
- Create: `src/permissions/mod.rs` — re-exports
- Create: `src/permissions/types.rs` — `PermissionTier`, `ToolKind`, `classify_tool`, `PermissionDecision`, `PermissionRequest`, `PermissionPrompter`
- Create: `src/permissions/settings.rs` — `PermissionSettings`, `SettingsFile`, `load_settings`
- Create: `src/permissions/gate.rs` — `PermissionGate`, `CheckOutcome`
- Create: `src/permissions/stdio.rs` — `StdioPrompter`
- Create: `src/agent/mod.rs` — re-exports
- Create: `src/agent/provider.rs` — `build_model`, `ProviderError`
- Create: `src/agent/tools.rs` — the six `#[tool_fn]` tools
- Create: `src/agent/gated_tool.rs` — `GatedTool<T>` (the canonical, project-wide permission
  enforcement wrapper)
- Create: `src/agent/build.rs` — `register_all_tools`, `build_agent`
- Create: `src/agent/headless.rs` — `run_headless`
- Modify: `src/cli/mod.rs` — add `-p/--prompt`, `--connection`, `--permission-mode` top-level flags, async `run`
- Modify: `src/main.rs` — `#[tokio::main] async fn main()`

---

### Task 1: Dependencies and module scaffolding

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/permissions/mod.rs`
- Create: `src/agent/mod.rs`

- [ ] **Step 1: Add dependencies to `Cargo.toml`**

Append to `[dependencies]` (leave every existing Phase 1 dependency line untouched):

```toml
daimon = { version = "0.16.0", features = ["openai", "ollama", "macros"] }
tokio = { version = "1", features = ["full"] }
serde_json = "1"
regex = "1"
walkdir = "2"
glob = "0.3"
```

- [ ] **Step 2: Run `cargo check` to confirm dependencies resolve**

Run: `cargo check`
Expected: builds (unused-code warnings only) — confirms `daimon`'s `openai`/`ollama`/`macros`
features compile against the vendored registry copy.

- [ ] **Step 3: Add module declarations to `src/lib.rs`**

```rust
pub mod config;
pub mod cli;
pub mod permissions;
pub mod agent;
```

- [ ] **Step 4: Create `src/permissions/mod.rs`**

```rust
pub mod types;
pub mod settings;
pub mod gate;
pub mod stdio;

pub use types::{
    classify_tool, PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
    ToolKind,
};
pub use settings::{load_settings, PermissionSettings, SettingsFile};
pub use gate::{CheckOutcome, PermissionGate};
pub use stdio::StdioPrompter;
```

- [ ] **Step 5: Create `src/agent/mod.rs`**

```rust
pub mod provider;
pub mod tools;
pub mod gated_tool;
pub mod build;
pub mod headless;

pub use provider::{build_model, ProviderError};
pub use gated_tool::GatedTool;
pub use build::{build_agent, register_all_tools};
pub use headless::run_headless;
```

- [ ] **Step 6: Create empty placeholder files so the crate compiles**

Create each of the following with only a doc comment (they are filled in by later tasks in this
plan, all within this same session — no task is left unfinished at the end of the plan):

`src/permissions/types.rs`:
```rust
//! Permission decision types, filled in by Task 3.
```

`src/permissions/settings.rs`:
```rust
//! Project/user permission settings (allow/deny lists), filled in by Task 3.
```

`src/permissions/gate.rs`:
```rust
//! The permission decision engine, filled in by Task 4.
```

`src/permissions/stdio.rs`:
```rust
//! Plain stdio permission prompter, filled in by Task 5.
```

`src/agent/provider.rs`:
```rust
//! Connection -> daimon Model construction, filled in by Task 2.
```

`src/agent/tools.rs`:
```rust
//! Built-in tools, filled in by Task 7.
```

`src/agent/gated_tool.rs`:
```rust
//! The canonical permission-gated Tool wrapper, filled in by Task 6.
```

`src/agent/build.rs`:
```rust
//! Agent construction, filled in by Task 8.
```

`src/agent/headless.rs`:
```rust
//! Headless `-p` prompt execution, filled in by Task 9.
```

- [ ] **Step 7: Run `cargo check` to confirm the scaffolding compiles**

Run: `cargo check`
Expected: PASS, no errors (the `pub use` lines in `mod.rs` will fail to resolve until later tasks
add the real items — so for this step only, temporarily comment out the `pub use` blocks added in
Steps 4–5, leaving just the `pub mod ...;` lines). Re-enable the `pub use` blocks task-by-task as
each task defines the corresponding items (Task 3 re-enables the `permissions` re-exports it
completes, Task 4 adds the `gate` ones, etc. — track this inline rather than as a separate step).

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/permissions src/agent
git commit -m "chore: scaffold permissions and agent modules, add daimon/tokio deps"
```

---

### Task 2: Connection -> `daimon` Model construction

**Files:**
- Create: `src/agent/provider.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/agent/provider.rs

use daimon::model::SharedModel;
use daimon::model::ollama::Ollama;
use daimon::model::openai::OpenAi;

use crate::config::connection::{Connection, ProviderKind};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("connection '{0}' has an empty base_url")]
    EmptyBaseUrl(String),
}

/// Builds a `daimon` `Model` (erased behind `SharedModel`) from a resolved `Connection`
/// and its (optional) API key. `OpenAiCompatible` connections use `daimon`'s generic
/// OpenAI-compatible provider pointed at `connection.base_url`; `Ollama` connections use
/// the dedicated Ollama provider. Later phases (`/model` switching) call this directly.
pub fn build_model(connection: &Connection, api_key: Option<String>) -> Result<SharedModel, ProviderError> {
    if connection.base_url.trim().is_empty() {
        return Err(ProviderError::EmptyBaseUrl(connection.name.clone()));
    }

    let model: SharedModel = match connection.provider {
        ProviderKind::OpenAiCompatible => {
            let key = api_key.unwrap_or_default();
            std::sync::Arc::new(
                OpenAi::with_api_key(connection.default_model.clone(), key)
                    .with_base_url(connection.base_url.clone()),
            )
        }
        ProviderKind::Ollama => std::sync::Arc::new(
            Ollama::new(connection.default_model.clone()).with_base_url(connection.base_url.clone()),
        ),
    };

    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai_connection() -> Connection {
        Connection {
            name: "local-vllm".into(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:8000/v1".into(),
            default_model: "qwen2.5-coder-32b".into(),
            models: vec![],
        }
    }

    fn ollama_connection() -> Connection {
        Connection {
            name: "home-ollama".into(),
            provider: ProviderKind::Ollama,
            base_url: "http://localhost:11434".into(),
            default_model: "llama3.1".into(),
            models: vec![],
        }
    }

    #[test]
    fn builds_openai_compatible_model_without_key() {
        let result = build_model(&openai_connection(), None);
        assert!(result.is_ok());
    }

    #[test]
    fn builds_openai_compatible_model_with_key() {
        let result = build_model(&openai_connection(), Some("sk-test".into()));
        assert!(result.is_ok());
    }

    #[test]
    fn builds_ollama_model() {
        let result = build_model(&ollama_connection(), None);
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_empty_base_url() {
        let mut conn = openai_connection();
        conn.base_url = "  ".into();
        let result = build_model(&conn, None);
        assert!(matches!(result, Err(ProviderError::EmptyBaseUrl(name)) if name == "local-vllm"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib agent::provider`
Expected: FAIL to compile (the placeholder file from Task 1 has none of this content yet — replace
it wholesale with the content above, then this step's "failure" is really "doesn't exist yet";
after replacing, proceed to Step 3).

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib agent::provider`
Expected: PASS (4 tests). These are construction-only tests (no network I/O) — behavior against a
real server is covered by the `#[ignore]`d integration tests in Task 10.

- [ ] **Step 4: Re-enable the provider re-export in `src/agent/mod.rs`**

Confirm `src/agent/mod.rs` (from Task 1) already contains:
```rust
pub use provider::{build_model, ProviderError};
```
Run `cargo check` to confirm this now resolves.

- [ ] **Step 5: Commit**

```bash
git add src/agent/provider.rs src/agent/mod.rs
git commit -m "feat: construct daimon Model from a Connection"
```

---

### Task 3: Permission types, tool classification, and settings TOML

**Files:**
- Create: `src/permissions/types.rs`
- Create: `src/permissions/settings.rs`

- [ ] **Step 1: Write the failing tests for `types.rs`**

```rust
// src/permissions/types.rs

use std::future::Future;
use std::pin::Pin;

/// How aggressively the agent may act without asking the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionTier {
    /// Every write/edit/bash call prompts (default).
    Ask,
    /// File writes/edits auto-approved; bash still prompts.
    AutoAcceptEdits,
    /// Nothing prompts.
    FullAuto,
}

/// Coarse classification of a tool call for permission purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Never mutates state, never prompts (`read_file`, `grep`, `glob`).
    ReadOnly,
    /// Mutates the filesystem (`write_file`, `edit_file`).
    Edit,
    /// Executes an arbitrary shell command (`bash`).
    Bash,
}

/// Classifies a tool by name for permission enforcement.
///
/// Unknown tool names (e.g. future MCP-provided tools) intentionally classify as
/// [`ToolKind::Edit`] rather than [`ToolKind::ReadOnly`] — the safe default is to
/// prompt for anything we don't explicitly know is read-only.
pub fn classify_tool(name: &str) -> ToolKind {
    match name {
        "read_file" | "grep" | "glob" => ToolKind::ReadOnly,
        "write_file" | "edit_file" => ToolKind::Edit,
        "bash" => ToolKind::Bash,
        _ => ToolKind::Edit,
    }
}

/// A human-readable description of a pending tool call, shown to the user by
/// whatever [`PermissionPrompter`] is in use.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub description: String,
    pub command_preview: Option<String>,
}

/// What the user decided in response to a [`PermissionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    AllowAlwaysThisSession,
    Deny { feedback: String },
}

/// Renders a [`PermissionRequest`] to the user and returns their decision.
///
/// Kept separate from [`crate::permissions::gate::PermissionGate`]'s decision logic
/// so the TUI phase can supply an `ntui`-rendering implementation without touching
/// the gate at all. Uses a boxed future (rather than an `impl Future` return, which
/// would not be object-safe) so implementations can be stored behind `Arc<dyn
/// PermissionPrompter>`.
pub trait PermissionPrompter: Send + Sync {
    fn prompt<'a>(
        &'a self,
        request: &'a PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_tools_classify_as_read_only() {
        assert_eq!(classify_tool("read_file"), ToolKind::ReadOnly);
        assert_eq!(classify_tool("grep"), ToolKind::ReadOnly);
        assert_eq!(classify_tool("glob"), ToolKind::ReadOnly);
    }

    #[test]
    fn write_tools_classify_as_edit() {
        assert_eq!(classify_tool("write_file"), ToolKind::Edit);
        assert_eq!(classify_tool("edit_file"), ToolKind::Edit);
    }

    #[test]
    fn bash_classifies_as_bash() {
        assert_eq!(classify_tool("bash"), ToolKind::Bash);
    }

    #[test]
    fn unknown_tool_defaults_to_edit() {
        assert_eq!(classify_tool("some_future_mcp_tool"), ToolKind::Edit);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib permissions::types`
Expected: replace the Task 1 placeholder with the content above; then PASS (4 tests).

- [ ] **Step 3: Write the failing tests for `settings.rs`**

```rust
// src/permissions/settings.rs

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PermissionSettings {
    #[serde(default)]
    pub always_allow: Vec<String>,
    #[serde(default)]
    pub always_deny: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsFile {
    #[serde(default)]
    pub permissions: PermissionSettings,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Loads `settings.toml` from both `user_config_dir` and `project_config_dir` and
/// unions their `always_allow`/`always_deny` lists (both layers are additive safety
/// hints — a rule present at either level applies). Missing files yield empty lists,
/// not an error.
pub fn load_settings(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<PermissionSettings, SettingsError> {
    let user = load_one(&user_config_dir.join("settings.toml"))?;
    let project = load_one(&project_config_dir.join("settings.toml"))?;

    let mut always_allow = user.permissions.always_allow;
    for rule in project.permissions.always_allow {
        if !always_allow.contains(&rule) {
            always_allow.push(rule);
        }
    }

    let mut always_deny = user.permissions.always_deny;
    for rule in project.permissions.always_deny {
        if !always_deny.contains(&rule) {
            always_deny.push(rule);
        }
    }

    Ok(PermissionSettings {
        always_allow,
        always_deny,
    })
}

fn load_one(path: &Path) -> Result<SettingsFile, SettingsError> {
    if !path.exists() {
        return Ok(SettingsFile::default());
    }
    let text = fs::read_to_string(path).map_err(|source| SettingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| SettingsError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("settings.toml"), contents).unwrap();
    }

    #[test]
    fn missing_files_yield_empty_lists() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let settings = load_settings(user_dir.path(), project_dir.path()).unwrap();
        assert!(settings.always_allow.is_empty());
        assert!(settings.always_deny.is_empty());
    }

    #[test]
    fn unions_rules_from_both_files() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        write(
            user_dir.path(),
            r#"
[permissions]
always_allow = ["cargo test"]
"#,
        );
        write(
            project_dir.path(),
            r#"
[permissions]
always_allow = ["cargo build"]
always_deny = ["rm -rf"]
"#,
        );

        let settings = load_settings(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(settings.always_allow, vec!["cargo test", "cargo build"]);
        assert_eq!(settings.always_deny, vec!["rm -rf"]);
    }

    #[test]
    fn deduplicates_rule_present_in_both_files() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        write(
            user_dir.path(),
            r#"
[permissions]
always_deny = ["rm -rf"]
"#,
        );
        write(
            project_dir.path(),
            r#"
[permissions]
always_deny = ["rm -rf"]
"#,
        );

        let settings = load_settings(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(settings.always_deny, vec!["rm -rf"]);
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail, then pass**

Run: `cargo test --lib permissions::settings`
Expected: replace the Task 1 placeholder with the content above; then PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/permissions/types.rs src/permissions/settings.rs
git commit -m "feat: add permission tiers, tool classification, and settings.toml loading"
```

---

### Task 4: `PermissionGate` decision engine

**Files:**
- Create: `src/permissions/gate.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/permissions/gate.rs

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::permissions::settings::PermissionSettings;
use crate::permissions::types::{
    classify_tool, PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
    ToolKind,
};

/// Result of [`PermissionGate::check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Allowed,
    /// Denied, with the reason/feedback to relay back to the model as the tool result.
    Denied(String),
}

/// The permission decision engine. Holds the current tier, the project/user
/// allow/deny list, per-session "don't ask again" state, and a pluggable
/// [`PermissionPrompter`]. Reused verbatim by the TUI phase (only the prompter
/// implementation changes).
pub struct PermissionGate {
    tier: Mutex<PermissionTier>,
    settings: PermissionSettings,
    session_allow: Mutex<HashSet<String>>,
    prompter: Arc<dyn PermissionPrompter>,
}

impl PermissionGate {
    pub fn new(
        tier: PermissionTier,
        settings: PermissionSettings,
        prompter: Arc<dyn PermissionPrompter>,
    ) -> Self {
        Self {
            tier: Mutex::new(tier),
            settings,
            session_allow: Mutex::new(HashSet::new()),
            prompter,
        }
    }

    pub async fn set_tier(&self, tier: PermissionTier) {
        *self.tier.lock().await = tier;
    }

    pub async fn tier(&self) -> PermissionTier {
        *self.tier.lock().await
    }

    /// Decides whether `tool_name` may execute with `arguments`. Read-only tools
    /// always return `Allowed`. Bash commands are checked against the always-deny
    /// list first (a hard boundary regardless of tier) and then the always-allow
    /// list (skips prompting regardless of tier). Otherwise the decision follows
    /// the current tier, prompting via [`PermissionPrompter`] when required.
    pub async fn check(&self, tool_name: &str, arguments: &serde_json::Value) -> CheckOutcome {
        let kind = classify_tool(tool_name);

        if kind == ToolKind::ReadOnly {
            return CheckOutcome::Allowed;
        }

        if kind == ToolKind::Bash {
            if let Some(command) = arguments.get("command").and_then(|v| v.as_str()) {
                if self
                    .settings
                    .always_deny
                    .iter()
                    .any(|rule| command.contains(rule.as_str()))
                {
                    return CheckOutcome::Denied(format!(
                        "command matches an always-deny rule and was blocked: {command}"
                    ));
                }
                if self
                    .settings
                    .always_allow
                    .iter()
                    .any(|rule| command.contains(rule.as_str()))
                {
                    return CheckOutcome::Allowed;
                }
            }
        }

        let tier = self.tier().await;
        match (tier, kind) {
            (PermissionTier::FullAuto, _) => CheckOutcome::Allowed,
            (PermissionTier::AutoAcceptEdits, ToolKind::Edit) => CheckOutcome::Allowed,
            _ => self.ask(tool_name, arguments).await,
        }
    }

    async fn ask(&self, tool_name: &str, arguments: &serde_json::Value) -> CheckOutcome {
        if self.session_allow.lock().await.contains(tool_name) {
            return CheckOutcome::Allowed;
        }

        let command_preview = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);
        let description = describe_call(tool_name, arguments);
        let request = PermissionRequest {
            tool_name: tool_name.to_string(),
            description,
            command_preview,
        };

        match self.prompter.prompt(&request).await {
            PermissionDecision::Allow => CheckOutcome::Allowed,
            PermissionDecision::AllowAlwaysThisSession => {
                self.session_allow.lock().await.insert(tool_name.to_string());
                CheckOutcome::Allowed
            }
            PermissionDecision::Deny { feedback } => CheckOutcome::Denied(feedback),
        }
    }
}

fn describe_call(tool_name: &str, arguments: &serde_json::Value) -> String {
    match tool_name {
        "bash" => format!(
            "run shell command: {}",
            arguments.get("command").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "write_file" => format!(
            "write file: {}",
            arguments.get("path").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "edit_file" => format!(
            "edit file: {}",
            arguments.get("path").and_then(|v| v.as_str()).unwrap_or("")
        ),
        other => format!("call tool '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubPrompter {
        decision: PermissionDecision,
    }

    impl PermissionPrompter for StubPrompter {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            let decision = self.decision.clone();
            Box::pin(async move { decision })
        }
    }

    fn gate_with(tier: PermissionTier, decision: PermissionDecision) -> PermissionGate {
        PermissionGate::new(
            tier,
            PermissionSettings::default(),
            Arc::new(StubPrompter { decision }),
        )
    }

    #[tokio::test]
    async fn read_only_tools_never_prompt_even_in_ask_tier() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let outcome = gate.check("read_file", &serde_json::json!({"path": "x"})).await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }

    #[tokio::test]
    async fn full_auto_allows_bash_without_prompting() {
        let gate = gate_with(
            PermissionTier::FullAuto,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let outcome = gate.check("bash", &serde_json::json!({"command": "ls"})).await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }

    #[tokio::test]
    async fn auto_accept_edits_allows_edit_but_still_prompts_bash() {
        let gate = gate_with(PermissionTier::AutoAcceptEdits, PermissionDecision::Allow);
        let edit_outcome = gate
            .check("write_file", &serde_json::json!({"path": "x", "content": "y"}))
            .await;
        assert_eq!(edit_outcome, CheckOutcome::Allowed);

        let gate_denying_bash = gate_with(
            PermissionTier::AutoAcceptEdits,
            PermissionDecision::Deny {
                feedback: "no".into(),
            },
        );
        let bash_outcome = gate_denying_bash
            .check("bash", &serde_json::json!({"command": "ls"}))
            .await;
        assert_eq!(bash_outcome, CheckOutcome::Denied("no".into()));
    }

    #[tokio::test]
    async fn ask_tier_denies_with_feedback() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "use a different approach".into(),
            },
        );
        let outcome = gate
            .check("edit_file", &serde_json::json!({"path": "x", "find": "a", "replace": "b"}))
            .await;
        assert_eq!(
            outcome,
            CheckOutcome::Denied("use a different approach".into())
        );
    }

    #[tokio::test]
    async fn allow_always_this_session_skips_future_prompts() {
        let gate = gate_with(PermissionTier::Ask, PermissionDecision::AllowAlwaysThisSession);
        let first = gate
            .check("bash", &serde_json::json!({"command": "cargo test"}))
            .await;
        assert_eq!(first, CheckOutcome::Allowed);

        // Second call would deny if the prompter were consulted again; instead the
        // gate's session_allow cache should short-circuit to Allowed.
        let gate_denying = PermissionGate::new(
            PermissionTier::Ask,
            PermissionSettings::default(),
            Arc::new(StubPrompter {
                decision: PermissionDecision::Deny {
                    feedback: "should not be reached".into(),
                },
            }),
        );
        gate_denying
            .session_allow
            .lock()
            .await
            .insert("bash".to_string());
        let second = gate_denying
            .check("bash", &serde_json::json!({"command": "cargo test"}))
            .await;
        assert_eq!(second, CheckOutcome::Allowed);
    }

    #[tokio::test]
    async fn always_deny_list_blocks_regardless_of_tier() {
        let mut settings = PermissionSettings::default();
        settings.always_deny.push("rm -rf".into());
        let gate = PermissionGate::new(
            PermissionTier::FullAuto,
            settings,
            Arc::new(StubPrompter {
                decision: PermissionDecision::Allow,
            }),
        );
        let outcome = gate
            .check("bash", &serde_json::json!({"command": "rm -rf /tmp/x"}))
            .await;
        assert!(matches!(outcome, CheckOutcome::Denied(_)));
    }

    #[tokio::test]
    async fn always_allow_list_skips_prompt_in_ask_tier() {
        let mut settings = PermissionSettings::default();
        settings.always_allow.push("cargo test".into());
        let gate = PermissionGate::new(
            PermissionTier::Ask,
            settings,
            Arc::new(StubPrompter {
                decision: PermissionDecision::Deny {
                    feedback: "should never be reached".into(),
                },
            }),
        );
        let outcome = gate
            .check("bash", &serde_json::json!({"command": "cargo test --lib"}))
            .await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib permissions::gate`
Expected: replace the Task 1 placeholder with the content above; then PASS (7 tests).

- [ ] **Step 3: Commit**

```bash
git add src/permissions/gate.rs
git commit -m "feat: add PermissionGate decision engine"
```

---

### Task 5: `StdioPrompter` — plain stdin/stdout permission prompts

**Files:**
- Create: `src/permissions/stdio.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/permissions/stdio.rs

use std::future::Future;
use std::pin::Pin;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest};

/// Renders a [`PermissionRequest`] as plain numbered choices over any
/// `AsyncBufRead`/`AsyncWrite` pair (real stdin/stdout in production, an in-memory
/// buffer in tests). The TUI phase will supply a different [`PermissionPrompter`]
/// impl that renders inline in the transcript instead — this type is not reused
/// there, but the trait it implements is.
pub struct StdioPrompter<R, W> {
    input: Mutex<R>,
    output: Mutex<W>,
}

impl<R, W> StdioPrompter<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(input: R, output: W) -> Self {
        Self {
            input: Mutex::new(input),
            output: Mutex::new(output),
        }
    }
}

impl StdioPrompter<tokio::io::BufReader<tokio::io::Stdin>, tokio::io::Stdout> {
    /// Convenience constructor wired to the real process stdin/stdout.
    pub fn real() -> Self {
        Self::new(tokio::io::BufReader::new(tokio::io::stdin()), tokio::io::stdout())
    }
}

impl<R, W> PermissionPrompter for StdioPrompter<R, W>
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    fn prompt<'a>(
        &'a self,
        request: &'a PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
        Box::pin(async move {
            let mut out = self.output.lock().await;
            let _ = out
                .write_all(
                    format!(
                        "\nPermission requested: {}\n  1) Yes\n  2) Yes, don't ask again this session\n  3) No (provide feedback)\n> ",
                        request.description
                    )
                    .as_bytes(),
                )
                .await;
            let _ = out.flush().await;
            drop(out);

            let mut input = self.input.lock().await;
            let mut line = String::new();
            if input.read_line(&mut line).await.is_err() {
                return PermissionDecision::Deny {
                    feedback: "failed to read permission response".into(),
                };
            }

            match line.trim() {
                "1" => PermissionDecision::Allow,
                "2" => PermissionDecision::AllowAlwaysThisSession,
                _ => {
                    let mut out = self.output.lock().await;
                    let _ = out.write_all(b"Feedback (why not / what to do instead): ").await;
                    let _ = out.flush().await;
                    drop(out);

                    let mut feedback = String::new();
                    let _ = input.read_line(&mut feedback).await;
                    PermissionDecision::Deny {
                        feedback: feedback.trim().to_string(),
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn request() -> PermissionRequest {
        PermissionRequest {
            tool_name: "bash".into(),
            description: "run shell command: rm file.txt".into(),
            command_preview: Some("rm file.txt".into()),
        }
    }

    #[tokio::test]
    async fn choice_1_allows() {
        let prompter = StdioPrompter::new(Cursor::new(b"1\n".to_vec()), Vec::new());
        let decision = prompter.prompt(&request()).await;
        assert_eq!(decision, PermissionDecision::Allow);
    }

    #[tokio::test]
    async fn choice_2_allows_always_this_session() {
        let prompter = StdioPrompter::new(Cursor::new(b"2\n".to_vec()), Vec::new());
        let decision = prompter.prompt(&request()).await;
        assert_eq!(decision, PermissionDecision::AllowAlwaysThisSession);
    }

    #[tokio::test]
    async fn choice_3_denies_with_feedback() {
        let prompter = StdioPrompter::new(
            Cursor::new(b"3\nplease use a temp file instead\n".to_vec()),
            Vec::new(),
        );
        let decision = prompter.prompt(&request()).await;
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                feedback: "please use a temp file instead".into()
            }
        );
    }

    #[tokio::test]
    async fn unrecognized_input_treated_as_deny() {
        let prompter = StdioPrompter::new(Cursor::new(b"garbage\nbecause reasons\n".to_vec()), Vec::new());
        let decision = prompter.prompt(&request()).await;
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                feedback: "because reasons".into()
            }
        );
    }

    #[tokio::test]
    async fn prompt_text_is_written_to_output() {
        let output = Vec::new();
        let prompter = StdioPrompter::new(Cursor::new(b"1\n".to_vec()), output);
        prompter.prompt(&request()).await;
        let written = prompter.output.lock().await;
        let text = String::from_utf8(written.clone()).unwrap();
        assert!(text.contains("run shell command: rm file.txt"));
        assert!(text.contains("Yes, don't ask again this session"));
    }
}
```

Note: `PermissionDecision` must derive `PartialEq`/`Eq` (already done in Task 3) for these
assertions to compile; `Cursor<Vec<u8>>` implements `AsyncRead` via `tokio`'s blanket impl over
`std::io::Read` only when wrapped — use `tokio::io::BufReader::new(Cursor::new(...))` if a plain
`Cursor` does not satisfy `AsyncBufRead` directly. Adjust the test helper to
`tokio::io::BufReader::new(Cursor::new(b"1\n".to_vec()))` if `cargo test` reports a missing
`AsyncBufRead` impl for `Cursor` — `tokio::io::BufReader<T: AsyncRead>` always implements
`AsyncBufRead`, and `tokio::io::AsyncRead` is implemented for any `T: std::io::Read` via
`tokio::io::AsyncFdReadyGuard`... in practice just wrap every test's `Cursor` in
`tokio::io::BufReader::new(...)` to be safe; do this consistently across all five tests above
before running them.

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib permissions::stdio`
Expected: replace the Task 1 placeholder with the content above (with the `BufReader::new(Cursor::new(...))`
wrapping applied per the note); then PASS (5 tests).

- [ ] **Step 3: Commit**

```bash
git add src/permissions/stdio.rs
git commit -m "feat: add StdioPrompter for plain-terminal permission prompts"
```

---

### Task 6: `GatedTool<T>` — the canonical, streaming-safe permission enforcement wrapper

**Files:**
- Create: `src/agent/gated_tool.rs`

This is the *one* permission-enforcement mechanism used everywhere in the project — headless
(`Agent::prompt`, this phase), the TUI (`Agent::prompt_stream`, Phase 3), and MCP tools (Phase 5).
It embeds the permission check *inside* each tool's own `execute()`, rather than relying on
`daimon::middleware::Middleware`. This is a deliberate, verified choice, not an oversight: reading
the real vendored `daimon-0.16.0` source (`src/agent/runner.rs`) shows that `Agent::prompt_stream`
calls `tool.execute_erased(...)` directly and never runs the `Middleware` stack, while `Agent::prompt`
does. A `Middleware`-based approach would therefore enforce permissions under `prompt` but silently
do nothing under `prompt_stream` — exactly the kind of two-mechanisms-that-can-drift-apart risk this
plan avoids by having exactly one mechanism, reused by every later phase without modification.

- [ ] **Step 1: Write the failing test**

```rust
// src/agent/gated_tool.rs

use std::sync::Arc;

use daimon::tool::{Tool, ToolOutput};

use crate::permissions::gate::{CheckOutcome, PermissionGate};

/// Wraps any `daimon::tool::Tool` so its own `execute` consults a
/// [`PermissionGate`] before doing real work. Both `daimon::agent::Agent::prompt`
/// and `Agent::prompt_stream` call a tool's `execute`/`execute_erased` to run it,
/// so embedding the check here (rather than in a `daimon::middleware::Middleware`,
/// which `prompt_stream` never invokes — confirmed by reading
/// `daimon-0.16.0/src/agent/runner.rs`) makes permission enforcement work
/// identically no matter which of the two the caller uses. This is the single
/// enforcement mechanism for the whole project: headless mode (this phase's
/// `build_agent`/`register_all_tools`), the TUI (Phase 3's `build_streaming_agent`),
/// and MCP tools (Phase 5's `NamespacedMcpTool`, wrapped exactly like a built-in)
/// all wrap every tool in `GatedTool` before registering it.
pub struct GatedTool<T> {
    inner: T,
    gate: Arc<PermissionGate>,
}

impl<T: Tool> GatedTool<T> {
    pub fn new(inner: T, gate: Arc<PermissionGate>) -> Self {
        Self { inner, gate }
    }
}

impl<T: Tool> Tool for GatedTool<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, input: &serde_json::Value) -> daimon::Result<ToolOutput> {
        match self.gate.check(self.inner.name(), input).await {
            CheckOutcome::Allowed => self.inner.execute(input).await,
            CheckOutcome::Denied(reason) => Ok(ToolOutput::error(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::{Bash, ReadFile, WriteFile};
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier};
    use std::future::Future;
    use std::pin::Pin;

    struct StubPrompter {
        decision: PermissionDecision,
    }
    impl PermissionPrompter for StubPrompter {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            let decision = self.decision.clone();
            Box::pin(async move { decision })
        }
    }

    fn gate_with(tier: PermissionTier, decision: PermissionDecision) -> Arc<PermissionGate> {
        Arc::new(PermissionGate::new(
            tier,
            PermissionSettings::default(),
            Arc::new(StubPrompter { decision }),
        ))
    }

    #[tokio::test]
    async fn read_only_gated_tool_never_prompts_and_executes() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let tool = GatedTool::new(ReadFile, gate);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.txt");
        std::fs::write(&file, "hello").unwrap();
        let output = tool
            .execute(&serde_json::json!({"path": file.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "hello");
    }

    #[tokio::test]
    async fn denied_edit_tool_never_touches_the_filesystem() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "no thanks".into(),
            },
        );
        let tool = GatedTool::new(WriteFile, gate);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.txt");
        let output = tool
            .execute(&serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "should not be written"
            }))
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(output.content, "no thanks");
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn allowed_bash_tool_executes() {
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        let tool = GatedTool::new(Bash, gate);
        let output = tool
            .execute(&serde_json::json!({"command": "echo gated_ok"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("gated_ok"));
    }
}
```

Note: this task's tests reference `crate::agent::tools::{Bash, ReadFile, WriteFile}`, which are
defined by Task 7 — write this task's test module after Task 7's tools exist (swap the order in
which you make these two tasks' tests pass if your workflow prefers tools before gating; the
plan lists `GatedTool` first only because Task 8 needs both).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib agent::gated_tool`
Expected: FAIL to compile — replace the Task 1 placeholder with the content above. If `agent::tools`
is not yet implemented (Task 7 not yet done), implement Task 7 first, then return to this step.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib agent::gated_tool`
Expected: PASS (3 tests). This is the load-bearing proof that permission enforcement survives
regardless of which `daimon::agent::Agent` entry point (`prompt` or `prompt_stream`) a caller uses —
`denied_edit_tool_never_touches_the_filesystem` in particular is the exact assertion Phase 3 will
later re-run (unchanged, just imported) against `Agent::prompt_stream` to prove the same guarantee
holds under streaming.

- [ ] **Step 4: Commit**

```bash
git add src/agent/gated_tool.rs
git commit -m "feat: add GatedTool, the one permission-enforcement wrapper used by every phase"
```

---

### Task 7: Built-in tools — `read_file`, `write_file`, `edit_file`, `bash`, `grep`, `glob`

**Files:**
- Modify: `src/agent/tools.rs`

- [ ] **Step 1: Write the failing tests, replacing the Task 1 placeholder in `src/agent/tools.rs`**

Replace the whole file's content (there is nothing to preserve — Task 1's placeholder is just a doc
comment):

```rust
//! Built-in tools: the six `#[tool_fn]`-annotated functions below. Permission
//! enforcement does not live in these bodies — see `crate::agent::gated_tool::GatedTool`,
//! which wraps each of these before registration.

use daimon::tool::ToolOutput;
use daimon::tool_fn;

/// Reads the full contents of a file at `path` (absolute or relative to the
/// process's current working directory).
#[tool_fn]
async fn read_file(
    /// Path to the file to read.
    path: String,
) -> daimon::Result<ToolOutput> {
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => Ok(ToolOutput::text(content)),
        Err(e) => Ok(ToolOutput::error(format!("failed to read {path}: {e}"))),
    }
}

/// Writes `content` to `path`, creating the file (and parent directories) if it
/// doesn't exist, or overwriting it entirely if it does. For targeted changes to
/// an existing file, prefer `edit_file`.
#[tool_fn]
async fn write_file(
    /// Path to the file to write.
    path: String,
    /// The full content to write to the file.
    content: String,
) -> daimon::Result<ToolOutput> {
    let path_ref = std::path::Path::new(&path);
    if let Some(parent) = path_ref.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Ok(ToolOutput::error(format!(
                    "failed to create parent directories for {path}: {e}"
                )));
            }
        }
    }
    match tokio::fs::write(&path, content).await {
        Ok(()) => Ok(ToolOutput::text(format!("wrote {path}"))),
        Err(e) => Ok(ToolOutput::error(format!("failed to write {path}: {e}"))),
    }
}

/// Replaces the single occurrence of `find` with `replace` inside the file at
/// `path`. Fails (without modifying the file) if `find` occurs zero times or more
/// than once, so the caller must supply enough surrounding context to make `find`
/// unique — this is a targeted find/replace, not a whole-file overwrite.
#[tool_fn]
async fn edit_file(
    /// Path to the file to edit.
    path: String,
    /// The exact text to find. Must occur exactly once in the file.
    find: String,
    /// The text to replace it with.
    replace: String,
) -> daimon::Result<ToolOutput> {
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) => return Ok(ToolOutput::error(format!("failed to read {path}: {e}"))),
    };

    let occurrences = content.matches(find.as_str()).count();
    if occurrences == 0 {
        return Ok(ToolOutput::error(format!(
            "find text not found in {path}"
        )));
    }
    if occurrences > 1 {
        return Ok(ToolOutput::error(format!(
            "find text is ambiguous in {path}: occurs {occurrences} times, expected exactly 1"
        )));
    }

    let updated = content.replacen(find.as_str(), &replace, 1);
    match tokio::fs::write(&path, updated).await {
        Ok(()) => Ok(ToolOutput::text(format!("edited {path}"))),
        Err(e) => Ok(ToolOutput::error(format!("failed to write {path}: {e}"))),
    }
}

/// Executes `command` as a shell command (`sh -c`) and returns combined stdout and
/// stderr along with the exit code. Subject to the active permission tier.
#[tool_fn]
async fn bash(
    /// The shell command to execute.
    command: String,
) -> daimon::Result<ToolOutput> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .output()
        .await;

    match output {
        Ok(out) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&out.stdout));
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            let exit_code = out.status.code().unwrap_or(-1);
            let text = format!("exit code: {exit_code}\n{combined}");
            if out.status.success() {
                Ok(ToolOutput::text(text))
            } else {
                Ok(ToolOutput::error(text))
            }
        }
        Err(e) => Ok(ToolOutput::error(format!("failed to execute command: {e}"))),
    }
}

/// Searches for lines matching a regular expression `pattern` inside files under
/// `path` (defaults to the current directory), recursively. Returns up to 200
/// matches as `file:line: text`.
#[tool_fn]
async fn grep(
    /// Regular expression to search for.
    pattern: String,
    /// Directory to search under. Defaults to the current directory if omitted.
    path: Option<String>,
) -> daimon::Result<ToolOutput> {
    let root = path.unwrap_or_else(|| ".".to_string());
    let regex = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(e) => return Ok(ToolOutput::error(format!("invalid regex '{pattern}': {e}"))),
    };

    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for (line_no, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(format!(
                    "{}:{}: {}",
                    entry.path().display(),
                    line_no + 1,
                    line
                ));
                if matches.len() >= 200 {
                    break;
                }
            }
        }
        if matches.len() >= 200 {
            break;
        }
    }

    if matches.is_empty() {
        Ok(ToolOutput::text("no matches found"))
    } else {
        Ok(ToolOutput::text(matches.join("\n")))
    }
}

/// Finds files whose path matches a glob `pattern` (e.g. `**/*.rs`) under `path`
/// (defaults to the current directory). Returns up to 200 matches, one per line.
#[tool_fn]
async fn glob(
    /// Glob pattern to match, relative to `path` (e.g. `**/*.rs`).
    pattern: String,
    /// Directory to search under. Defaults to the current directory if omitted.
    path: Option<String>,
) -> daimon::Result<ToolOutput> {
    let root = path.unwrap_or_else(|| ".".to_string());
    let full_pattern = format!("{}/{}", root.trim_end_matches('/'), pattern);

    let paths = match glob::glob(&full_pattern) {
        Ok(p) => p,
        Err(e) => return Ok(ToolOutput::error(format!("invalid glob '{full_pattern}': {e}"))),
    };

    let mut matches = Vec::new();
    for entry in paths {
        if let Ok(p) = entry {
            matches.push(p.display().to_string());
            if matches.len() >= 200 {
                break;
            }
        }
    }

    if matches.is_empty() {
        Ok(ToolOutput::text("no matches found"))
    } else {
        Ok(ToolOutput::text(matches.join("\n")))
    }
}

#[cfg(test)]
mod builtin_tool_tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn read_file_returns_contents() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let tool = ReadFile;
        let output = tool
            .execute(&serde_json::json!({"path": file_path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "hello world");
    }

    #[tokio::test]
    async fn read_file_missing_file_is_an_error_output_not_a_panic() {
        let tool = ReadFile;
        let output = tool
            .execute(&serde_json::json!({"path": "/nonexistent/path/x.txt"}))
            .await
            .unwrap();
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn write_file_creates_file_and_parent_dirs() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("nested").join("out.txt");

        let tool = WriteFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "content": "new content"
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "new content");
    }

    #[tokio::test]
    async fn edit_file_replaces_unique_match() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        std::fs::write(&file_path, "fn main() { println!(\"old\"); }").unwrap();

        let tool = EditFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "find": "old",
                "replace": "new"
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "fn main() { println!(\"new\"); }"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_ambiguous_match() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        std::fs::write(&file_path, "a a a").unwrap();

        let tool = EditFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "find": "a",
                "replace": "b"
            }))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("ambiguous"));
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "a a a");
    }

    #[tokio::test]
    async fn edit_file_rejects_missing_match() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("code.rs");
        std::fs::write(&file_path, "content").unwrap();

        let tool = EditFile;
        let output = tool
            .execute(&serde_json::json!({
                "path": file_path.to_str().unwrap(),
                "find": "not present",
                "replace": "x"
            }))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("not found"));
    }

    #[tokio::test]
    async fn bash_returns_stdout_and_success_exit_code() {
        let tool = Bash;
        let output = tool
            .execute(&serde_json::json!({"command": "echo hello_from_bash_tool"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("hello_from_bash_tool"));
        assert!(output.content.contains("exit code: 0"));
    }

    #[tokio::test]
    async fn bash_reports_nonzero_exit_as_error_output() {
        let tool = Bash;
        let output = tool.execute(&serde_json::json!({"command": "exit 3"})).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("exit code: 3"));
    }

    #[tokio::test]
    async fn grep_finds_matching_lines() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world\nfoo bar\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing here\n").unwrap();

        let tool = Grep;
        let output = tool
            .execute(&serde_json::json!({
                "pattern": "hello",
                "path": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("hello world"));
        assert!(!output.content.contains("nothing here"));
    }

    #[tokio::test]
    async fn grep_reports_no_matches() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "nothing relevant\n").unwrap();

        let tool = Grep;
        let output = tool
            .execute(&serde_json::json!({
                "pattern": "unmatchable_pattern_xyz",
                "path": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "no matches found");
    }

    #[tokio::test]
    async fn glob_finds_matching_files() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("one.rs"), "").unwrap();
        std::fs::write(dir.path().join("two.txt"), "").unwrap();

        let tool = Glob;
        let output = tool
            .execute(&serde_json::json!({
                "pattern": "*.rs",
                "path": dir.path().to_str().unwrap()
            }))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("one.rs"));
        assert!(!output.content.contains("two.txt"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib agent::tools`
Expected: FAIL to compile initially — the code above defines the six `#[tool_fn]` functions and
their generated `ReadFile`/`WriteFile`/`EditFile`/`Bash`/`Grep`/`Glob` structs together with their
tests in the same edit (consistent with how `#[tool_fn]` must be exercised: the macro-generated
struct only exists once the annotated function is compiled). After adding the full content above,
proceed to Step 3.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib agent::tools`
Expected: PASS (12 tests in this module). Task 6's `GatedTool` tests, in
`src/agent/gated_tool.rs`, import `ReadFile`/`WriteFile`/`Bash` from here — run
`cargo test --lib agent::gated_tool` too once both tasks are done, to confirm those 3 tests also
pass.

- [ ] **Step 4: Commit**

```bash
git add src/agent/tools.rs
git commit -m "feat: add read_file/write_file/edit_file/bash/grep/glob built-in tools"
```

---

### Task 8: `register_all_tools`/`build_agent` — wiring model and `GatedTool`-wrapped tools together

**Files:**
- Create: `src/agent/build.rs`

`register_all_tools` is the single shared function that registers every available tool onto an
`AgentBuilder`. In this phase it only knows about the six built-ins (wrapped in `GatedTool`); Phase
5 (MCP client) later extends its signature in place to also register MCP-discovered tools (see this
plan's Self-review notes), and Phase 4 (TUI slash commands/persistence) calls this exact function
from its own agent-rebuild path (`build_streaming_agent_with_history`) so the TUI and headless mode
can never register a different tool set from each other.

- [ ] **Step 1: Write the failing test**

```rust
// src/agent/build.rs

use std::sync::Arc;

use daimon::agent::{Agent, AgentBuilder};
use daimon::model::SharedModel;

use crate::agent::gated_tool::GatedTool;
use crate::agent::tools::{Bash, EditFile, Glob, Grep, ReadFile, WriteFile};
use crate::permissions::gate::PermissionGate;

const DEFAULT_SYSTEM_PROMPT: &str = "You are local-code, a coding assistant that talks only to \
local/local-network LLM backends. You can read, write, and edit files, run shell commands, and \
search the codebase via your tools. Prefer edit_file for targeted changes over rewriting whole \
files with write_file. Always explain what you're about to do before calling a tool that changes \
the filesystem or runs a command.";

/// Registers every available tool onto `builder`, each wrapped in
/// [`GatedTool`] so permission enforcement works identically whether the
/// resulting `Agent` is later driven via `prompt` or `prompt_stream`. This
/// phase's version registers only the six built-ins; Phase 5 (MCP client)
/// extends this function's signature in place to add MCP-discovered tools
/// (each also `GatedTool`-wrapped) — see this plan's Self-review notes for the
/// explicit follow-up this leaves. Both the headless path (`build_agent`,
/// below) and the TUI path (Phase 4's `build_streaming_agent_with_history`)
/// call this one function, so they never drift apart.
pub fn register_all_tools(builder: AgentBuilder, gate: Arc<PermissionGate>) -> AgentBuilder {
    builder
        .tool(GatedTool::new(ReadFile, gate.clone()))
        .tool(GatedTool::new(WriteFile, gate.clone()))
        .tool(GatedTool::new(EditFile, gate.clone()))
        .tool(GatedTool::new(Bash, gate.clone()))
        .tool(GatedTool::new(Grep, gate.clone()))
        .tool(GatedTool::new(Glob, gate))
}

/// Builds a `daimon::agent::Agent` wired with the six `GatedTool`-wrapped
/// built-in tools via [`register_all_tools`]. No `daimon::middleware::Middleware`
/// is used anywhere — see `src/agent/gated_tool.rs` for why.
pub fn build_agent(model: SharedModel, gate: Arc<PermissionGate>) -> daimon::Result<Agent> {
    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(DEFAULT_SYSTEM_PROMPT);
    register_all_tools(builder, gate).build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier};
    use daimon::model::types::{ChatRequest, ChatResponse, Message, StopReason, Usage};
    use daimon::stream::ResponseStream;
    use std::future::Future;
    use std::pin::Pin;

    struct EchoModel;

    impl daimon::model::Model for EchoModel {
        async fn generate(&self, request: &ChatRequest) -> daimon::Result<ChatResponse> {
            let last = request
                .messages
                .last()
                .and_then(|m| m.content.as_deref())
                .unwrap_or("");
            Ok(ChatResponse {
                message: Message::assistant(format!("echo: {last}")),
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage::default()),
            })
        }

        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    struct AlwaysAllowPrompter;

    impl PermissionPrompter for AlwaysAllowPrompter {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            Box::pin(async { PermissionDecision::Allow })
        }
    }

    fn test_gate() -> Arc<PermissionGate> {
        Arc::new(PermissionGate::new(
            PermissionTier::FullAuto,
            PermissionSettings::default(),
            Arc::new(AlwaysAllowPrompter),
        ))
    }

    #[test]
    fn builds_successfully_with_all_six_tools_registered() {
        let model: SharedModel = Arc::new(EchoModel);
        let agent = build_agent(model, test_gate());
        assert!(agent.is_ok());
    }

    #[tokio::test]
    async fn built_agent_responds_to_a_simple_prompt() {
        let model: SharedModel = Arc::new(EchoModel);
        let agent = build_agent(model, test_gate()).unwrap();
        let response = agent.prompt("hello").await.unwrap();
        assert!(response.text().contains("echo: hello"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib agent::build`
Expected: FAIL to compile — replace the Task 1 placeholder with the content above.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib agent::build`
Expected: PASS (2 tests). Add `futures = "0.3"` to `[dependencies]` in `Cargo.toml` if it is not
already present transitively-visible (daimon depends on it, but our crate must declare it directly
to use `futures::stream::empty()` in our own test code) — run `cargo add futures` or add the line
manually, then re-run.

- [ ] **Step 4: Commit**

```bash
git add src/agent/build.rs Cargo.toml Cargo.lock
git commit -m "feat: wire model and GatedTool-wrapped built-in tools into an Agent via register_all_tools"
```

---

### Task 9: Headless `-p "<prompt>"` execution

**Files:**
- Create: `src/agent/headless.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/agent/headless.rs

use std::path::Path;
use std::sync::Arc;

use crate::agent::build::build_agent;
use crate::agent::provider::build_model;
use crate::config::connection::{load_connections, Connection};
use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use crate::permissions::gate::PermissionGate;
use crate::permissions::settings::load_settings;
use crate::permissions::stdio::StdioPrompter;
use crate::permissions::types::PermissionTier;

#[derive(Debug, thiserror::Error)]
pub enum HeadlessError {
    #[error("no connections configured; run `local-code connections add` first")]
    NoConnections,
    #[error("connection '{0}' not found")]
    ConnectionNotFound(String),
    #[error("multiple connections configured ({0}); pass --connection <name> to choose one")]
    AmbiguousConnection(String),
    #[error("failed to load connections: {0}")]
    LoadConnections(#[from] crate::config::connection::ConnectionsError),
    #[error("failed to load settings: {0}")]
    LoadSettings(#[from] crate::permissions::settings::SettingsError),
    #[error("failed to read API key: {0}")]
    Secrets(#[from] crate::config::secrets::SecretsError),
    #[error("failed to construct model: {0}")]
    Provider(#[from] crate::agent::provider::ProviderError),
    #[error("agent error: {0}")]
    Agent(#[from] daimon::DaimonError),
}

fn select_connection(
    connections: &[Connection],
    requested_name: Option<&str>,
) -> Result<Connection, HeadlessError> {
    if let Some(name) = requested_name {
        return connections
            .iter()
            .find(|c| c.name == name)
            .cloned()
            .ok_or_else(|| HeadlessError::ConnectionNotFound(name.to_string()));
    }
    match connections.len() {
        0 => Err(HeadlessError::NoConnections),
        1 => Ok(connections[0].clone()),
        _ => Err(HeadlessError::AmbiguousConnection(
            connections
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// Runs one full ReAct-loop turn headlessly and returns the final text response.
/// Headless invocations default to `PermissionTier::FullAuto` (there is no TTY to
/// answer an inline prompt); pass `permission_mode_override` to force a different
/// tier (the project/user allow/deny list still applies as a hard boundary
/// regardless of tier).
pub async fn run_headless(
    paths: &Paths,
    _project_root: &Path,
    connection_name: Option<&str>,
    permission_mode_override: Option<PermissionTier>,
    prompt: &str,
) -> Result<String, HeadlessError> {
    let connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    let connection = select_connection(&connections, connection_name)?;

    let api_key = SecretStore::get_api_key(&connection.name)?;
    let model = build_model(&connection, api_key)?;

    let settings = load_settings(&paths.user_config_dir, &paths.project_config_dir)?;
    let tier = permission_mode_override.unwrap_or(PermissionTier::FullAuto);
    let gate = Arc::new(PermissionGate::new(
        tier,
        settings,
        Arc::new(StdioPrompter::real()),
    ));

    let agent = build_agent(model, gate)?;
    let response = agent.prompt(prompt).await?;
    Ok(response.text().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::connection::ProviderKind;

    fn conn(name: &str) -> Connection {
        Connection {
            name: name.to_string(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:8000/v1".into(),
            default_model: "m".into(),
            models: vec![],
        }
    }

    #[test]
    fn select_connection_errors_when_none_configured() {
        let result = select_connection(&[], None);
        assert!(matches!(result, Err(HeadlessError::NoConnections)));
    }

    #[test]
    fn select_connection_picks_the_only_one_when_unambiguous() {
        let connections = vec![conn("only-one")];
        let result = select_connection(&connections, None).unwrap();
        assert_eq!(result.name, "only-one");
    }

    #[test]
    fn select_connection_errors_when_ambiguous_without_a_name() {
        let connections = vec![conn("a"), conn("b")];
        let result = select_connection(&connections, None);
        assert!(matches!(result, Err(HeadlessError::AmbiguousConnection(_))));
    }

    #[test]
    fn select_connection_finds_by_explicit_name() {
        let connections = vec![conn("a"), conn("b")];
        let result = select_connection(&connections, Some("b")).unwrap();
        assert_eq!(result.name, "b");
    }

    #[test]
    fn select_connection_errors_when_named_connection_missing() {
        let connections = vec![conn("a")];
        let result = select_connection(&connections, Some("does-not-exist"));
        assert!(matches!(result, Err(HeadlessError::ConnectionNotFound(name)) if name == "does-not-exist"));
    }
}
```

This requires `Connection` to derive `Clone` (already true from Phase 1) and
`ConnectionsError`/`SecretsError` to be `pub` from their modules (already true from Phase 1's
`src/config/connection.rs` and `src/config/secrets.rs`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib agent::headless`
Expected: FAIL to compile — replace the Task 1 placeholder with the content above.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib agent::headless`
Expected: PASS (5 tests, all pure connection-selection logic — no network I/O, no real model
construction is exercised here since that's covered by Task 2's tests and Task 10's ignored
integration tests).

- [ ] **Step 4: Commit**

```bash
git add src/agent/headless.rs
git commit -m "feat: add headless run_headless for the -p CLI flag"
```

---

### Task 10: Wire `-p`/`--connection`/`--permission-mode` into the CLI, async `main`

**Files:**
- Modify: `src/cli/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/cli/mod.rs` a new test module exercising the new arg-parsing surface (the
`Cli`/`PermissionModeArg` types below don't exist yet, so this fails to compile until Step 3):

```rust
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
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib cli::headless_cli_tests`
Expected: FAIL to compile (`prompt`/`connection`/`permission_mode` fields and `PermissionModeArg`
don't exist on `Cli` yet).

- [ ] **Step 3: Replace `src/cli/mod.rs` in full**

```rust
pub mod connections;

use crate::agent::headless::run_headless;
use crate::config::paths::Paths;
use crate::permissions::types::PermissionTier;
use clap::{Parser, Subcommand, ValueEnum};
use std::io::{stdin, stdout};
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
}

#[derive(Subcommand)]
pub enum ConnectionsAction {
    Add,
    List,
    Remove { name: String },
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

pub async fn run(cli: Cli, project_root: PathBuf) -> anyhow::Result<()> {
    let paths = Paths::resolve(&project_root)?;

    if let Some(prompt) = cli.prompt.as_deref() {
        let tier_override = cli.permission_mode.map(PermissionModeArg::into_tier);
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
        None => {
            println!(
                "local-code: no command given. Try `local-code -p \"<prompt>\"` or `local-code connections list`."
            );
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Replace `src/main.rs`**

```rust
use clap::Parser;
use local_code::cli::{run, Cli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let project_root = std::env::current_dir()?;
    run(cli, project_root).await
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib cli`
Expected: PASS — the 4 new `headless_cli_tests` plus all pre-existing `cli::connections` tests
from Phase 1 (which are untouched by this change and must still compile/pass, since `run` is now
`async` but `connections::add`/`list`/`remove` remain synchronous free functions called from
inside the async `run`).

- [ ] **Step 6: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS, all tests across every module from this plan and Phase 1.

- [ ] **Step 7: Manually verify the headless CLI end-to-end (requires a real local server)**

This step is documentation for manual verification, not an automated test — run it against a real
llama.cpp/vLLM/LM Studio server if one is available:

```bash
printf 'my-server\n1\nhttp://localhost:8000/v1\nqwen2.5-coder-7b\n\n' | cargo run -- connections add
cargo run -- -p "what is 2 + 2?"
```

Expected: prints the model's final text answer after the ReAct loop completes (zero or more tool
calls, then plain text).

- [ ] **Step 8: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "feat: add headless -p/--connection/--permission-mode CLI flags"
```

---

### Task 11: Ignored integration tests against real local servers

**Files:**
- Create: `tests/live_openai_compatible.rs`
- Create: `tests/live_ollama.rs`

These exercise the full stack (`build_model` → `build_agent` → `agent.prompt(...)`) against a
real running backend. They are `#[ignore]`d because CI and most dev machines won't have a local
LLM server listening — run them manually with `cargo test --test live_openai_compatible -- --ignored`
once a server is up.

- [ ] **Step 1: Write `tests/live_openai_compatible.rs`**

```rust
//! Integration test against a real OpenAI-compatible local server (llama.cpp
//! server, vLLM, LM Studio, text-generation-webui). Requires:
//!   - a server listening at `LOCAL_CODE_TEST_OPENAI_BASE_URL` (default
//!     `http://localhost:8000/v1`) that supports native OpenAI-style `tool_calls`.
//!   - `LOCAL_CODE_TEST_OPENAI_MODEL` set to a model ID the server has loaded.
//! Run with: `cargo test --test live_openai_compatible -- --ignored --nocapture`

use std::sync::Arc;

use local_code::agent::build::build_agent;
use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};
use local_code::permissions::gate::PermissionGate;
use local_code::permissions::settings::PermissionSettings;
use local_code::permissions::stdio::StdioPrompter;
use local_code::permissions::types::PermissionTier;

#[tokio::test]
#[ignore = "requires a real local OpenAI-compatible server with tool_calls support"]
async fn prompts_a_real_openai_compatible_server_and_gets_a_text_response() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OPENAI_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OPENAI_MODEL")
        .expect("set LOCAL_CODE_TEST_OPENAI_MODEL to a model your server has loaded");

    let connection = Connection {
        name: "live-test".into(),
        provider: ProviderKind::OpenAiCompatible,
        base_url,
        default_model: model_id,
        models: vec![],
    };

    let model = build_model(&connection, None).expect("model construction should not fail");
    let gate = Arc::new(PermissionGate::new(
        PermissionTier::FullAuto,
        PermissionSettings::default(),
        Arc::new(StdioPrompter::real()),
    ));
    let agent = build_agent(model, gate).expect("agent construction should not fail");

    let response = agent
        .prompt("Reply with exactly the word: pong")
        .await
        .expect("prompt should succeed against a live server");

    assert!(!response.text().is_empty());
}
```

- [ ] **Step 2: Write `tests/live_ollama.rs`**

```rust
//! Integration test against a real Ollama server. Requires:
//!   - Ollama running locally (default `http://localhost:11434`) with a model
//!     pulled that supports tool calling (e.g. `llama3.1`).
//!   - `LOCAL_CODE_TEST_OLLAMA_MODEL` set to that model's name.
//! Run with: `cargo test --test live_ollama -- --ignored --nocapture`

use std::sync::Arc;

use local_code::agent::build::build_agent;
use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};
use local_code::permissions::gate::PermissionGate;
use local_code::permissions::settings::PermissionSettings;
use local_code::permissions::stdio::StdioPrompter;
use local_code::permissions::types::PermissionTier;

#[tokio::test]
#[ignore = "requires a real local Ollama server with a tool-calling-capable model pulled"]
async fn prompts_a_real_ollama_server_and_gets_a_text_response() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OLLAMA_MODEL")
        .unwrap_or_else(|_| "llama3.1".to_string());

    let connection = Connection {
        name: "live-ollama-test".into(),
        provider: ProviderKind::Ollama,
        base_url,
        default_model: model_id,
        models: vec![],
    };

    let model = build_model(&connection, None).expect("model construction should not fail");
    let gate = Arc::new(PermissionGate::new(
        PermissionTier::FullAuto,
        PermissionSettings::default(),
        Arc::new(StdioPrompter::real()),
    ));
    let agent = build_agent(model, gate).expect("agent construction should not fail");

    let response = agent
        .prompt("Reply with exactly the word: pong")
        .await
        .expect("prompt should succeed against a live server");

    assert!(!response.text().is_empty());
}
```

- [ ] **Step 3: Confirm both compile and are skipped by default**

Run: `cargo test`
Expected: both new test binaries build; their tests are reported as `ignored`, not run, and the
rest of the suite still passes.

Run: `cargo test -- --ignored --list`
Expected: lists `prompts_a_real_openai_compatible_server_and_gets_a_text_response` and
`prompts_a_real_ollama_server_and_gets_a_text_response` as available ignored tests.

- [ ] **Step 4: Commit**

```bash
git add tests/live_openai_compatible.rs tests/live_ollama.rs
git commit -m "test: add ignored live integration tests against real OpenAI-compatible and Ollama servers"
```

---

## Self-review notes

- **Spec coverage:**
  - Section 2 (agent loop): `daimon::Agent` ReAct loop wired via `build_agent` (Task 8); both
    providers constructed via `build_model` (Task 2); all six built-in tools implemented via
    `#[tool_fn]` (Task 7); v1's native-`tool_calls`-only requirement is satisfied by construction
    (no prompt-parsed fallback is implemented anywhere in this plan).
  - Section 3 (permissions): all three tiers implemented and tested (Task 4); project/user
    allow/deny list in `.local-code/settings.toml` implemented and tested (Task 3); `ask` tier's
    "Yes / Yes don't ask again this session / No + feedback" implemented over stdio (Task 5) with
    the decision logic (`PermissionGate`, `PermissionPrompter` trait) fully decoupled from
    rendering so the TUI phase can supply its own prompter with no changes to `PermissionGate`.
  - Section 8 (headless invocation): `-p "<prompt>"` implemented (Task 10), defaults to
    `full-auto` (Task 9's `run_headless`), overridable via `--permission-mode` (Task 10), and the
    allow/deny list is still consulted even at `full-auto` (`PermissionGate::check` checks
    `always_deny` before considering tier — Task 4).
  - Explicitly out of scope and not touched: TUI/`ntui` (Task list never imports `ntui`), slash
    commands, MCP tool wiring (though `register_all_tools`, Task 8, is a plain function taking an
    `AgentBuilder` that Phase 5 extends in place to register MCP-discovered tools too — see the
    explicit Phase-5 follow-up note below), session persistence/`/compact`/`/resume`, and
    AGENTS.md/CLAUDE.md loading (headless mode uses one hardcoded system prompt string, called out
    explicitly in Task 9 and the traceability section).

- **No placeholders:** every `todo!()`/stub is confined to Task 1's scaffolding step and is fully
  replaced by the corresponding task's implementation later in this same plan (Task 1 Step 6
  placeholders → Task 2/3/4/5/6/7/8/9 replace each file in turn). Grep the finished plan for
  `TODO`, `TBD`, `unimplemented!`, and "implement later" — none should remain outside Task 1's
  Step 6 placeholder doc-comments, which are all overwritten by name in later tasks.

- **Type consistency:** `PermissionTier`, `ToolKind`, `classify_tool`, `PermissionDecision`,
  `PermissionRequest`, `PermissionPrompter` (Task 3) are defined once and reused verbatim by
  `PermissionGate` (Task 4), `StdioPrompter` (Task 5), `GatedTool` (Task 6), and the CLI
  (Task 10). `PermissionSettings`/`SettingsFile`/`load_settings` (Task 3) are reused by
  `PermissionGate::new` (Task 4) and `run_headless` (Task 9). `build_model`/`ProviderError` (Task
  2) are reused by `run_headless` (Task 9) and both live integration tests (Task 11). `GatedTool`
  (Task 6) is reused verbatim by Phase 3 (`build_streaming_agent`), Phase 4
  (`build_streaming_agent_with_history`), and Phase 5 (MCP tools). `register_all_tools`/`build_agent`
  (Task 8) are reused by `run_headless` (Task 9), both live integration tests (Task 11), Phase 4's
  TUI agent-rebuild path, and Phase 5's MCP-tool wiring — all of these import `register_all_tools`
  rather than reimplementing the tool-registration loop. All imports of Phase 1 types (`Paths`,
  `Connection`, `ProviderKind`, `ConnectionsFile`, `load_connections`, `SecretStore`) match the exact
  names/paths defined in `docs/superpowers/plans/2026-07-06-foundation-config-connections.md` with no
  redefinition.

- **Follow-up this plan explicitly leaves for Phase 5 (do not silently forget):**
  `register_all_tools` (Task 8) as defined in this phase only registers the six built-in tools — it
  has no way to know about MCP servers yet, since `local_code::mcp::tool::NamespacedMcpTool` and
  `local_code::mcp::connect::connect_all` are Phase 5 types that don't exist during this phase's
  implementation. Phase 5's own plan (`docs/superpowers/plans/2026-07-06-mcp-client.md`) must extend
  `register_all_tools`'s signature *in place* (add an `mcp_tools: Vec<NamespacedMcpTool>` parameter,
  wrap each in `GatedTool` exactly like the built-ins) rather than introduce a second, competing
  registration function — this is called out explicitly in Phase 5's own Task 8, which is the task
  that must land before this follow-up is considered resolved. Until Phase 5 is implemented,
  `register_all_tools`/`build_agent` only ever produce an agent with the six built-ins, which is
  correct and complete for this phase's own scope (headless mode with no MCP servers configured).

- **API-compatibility risks worth flagging before implementation starts:**
  1. **`#[tool_fn]` generates zero-field unit structs.** This is why permission enforcement cannot
     live inside the macro-generated struct's own definition — it has to be layered on top via a
     wrapper type instead. This is a real constraint of the vendored macro (confirmed by reading
     `daimon-macros-0.16.0/src/lib.rs`), not a simplification of choice. `GatedTool<T>` (Task 6) is
     that wrapper, and is deliberately *not* implemented as a `daimon::middleware::Middleware`: reading
     `daimon-0.16.0/src/agent/runner.rs` confirms `Agent::prompt_stream` calls
     `tool.execute_erased(...)` directly and never runs the `Middleware` stack (only `Agent::prompt`
     does), and separately, a `MiddlewareAction::ShortCircuit(_)` returned from `on_tool_call` is
     *ignored* by `execute_tools_parallel` — the call is always replaced with the literal text
     `"skipped by middleware"` regardless of what the middleware returned, discarding any real denial
     reason. Both of these are real defects in relying on `Middleware` for this purpose, and both are
     why this plan uses `GatedTool` (embedding the check inside `execute()`, which every code path
     calls) instead, uniformly, everywhere.
  2. **No connection/timeout-tuning surface exposed yet.** `daimon`'s `OpenAi`/`Ollama` providers
     support `with_timeout`/`with_max_retries`/`with_keep_alive`, but this plan's `build_model`
     doesn't expose them (no such fields exist on Phase 1's `Connection` type). Fine for v1; a
     later phase should extend `Connection` if per-connection timeout tuning becomes necessary.
  3. **`OpenAi::with_api_key` always sends an `Authorization: Bearer <key>` header**, even when
     `key` is an empty string (Task 2's `build_model` passes `api_key.unwrap_or_default()`).
     Confirmed via `daimon-0.16.0/src/model/openai.rs`: the header is unconditional. Most local
     servers ignore an empty/absent bearer token, but this is worth knowing if a specific backend
     rejects malformed `Authorization` headers outright.
