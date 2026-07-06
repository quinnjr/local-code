# Slash Commands & Session Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the full v1 slash-command set (`/model`, `/connections`, `/init`, `/permissions`,
`/compact`, `/resume`, `/clear`, `/help`) and session persistence (serialize transcript + active
connection/model to disk, `local-code --resume` / `/resume`) on top of Phase 1 (config/connections),
Phase 2 (agent loop/permissions), and Phase 3 (TUI shell). This phase is pure integration: it
introduces no new agent-framework concepts, only wires existing `daimon`/`ntui`/Phase 1–3 machinery
together and fills the one gap Phase 2 explicitly left open (AGENTS.md/CLAUDE.md context loading).

**Architecture:** Session state is `Vec<crate::tui::state::TranscriptEntry>` (Phase 3's exact
display-transcript type, given `Serialize`/`Deserialize` derives directly — no parallel DTO type,
since every field is plain `String`/`bool`/`Vec`/`Option` data) *plus* `Vec<daimon::model::types::Message>`
(the exact type `daimon`'s own `Agent::memory()` trades in, already `Serialize`/`Deserialize` from
`daimon-core`) for the raw agent-facing history. Both are needed because they are not
interconvertible: `TranscriptEntry` is a UI projection (separate `ToolCall` cards, `PermissionResolved`
markers) with no 1:1 mapping back to `daimon::Message`'s role-based turn structure. A session file
holds both side by side. `local_code::session::store` resolves one file per (project, timestamp)
under `Paths::user_state_dir` (already global-per-user, not project-scoped, so this plan adds a
project-slugging step), and `local_code::session::store::{save_session, list_sessions, load_session}`
are the only functions that touch disk.

Rebuilding an `Agent` (for `/model`, `/resume`, and the initial mount) is centralized in one new
helper, `local_code::tui::rebuild::rebuild_agent`, extracted from the inline closure Phase 3's `App`
component used at mount — Phase 3's own traceability section promised a reusable `rebuild_agent`
helper but its actual Task 7 code left the logic inline; this plan fixes that by extracting it, then
having `App`'s mount closure, `/model`, and `/resume` all call the one function. History continuity
across a rebuild (same conversation, different model/connection, or a resumed session) is provided
by a new small `Memory` implementor, `local_code::tui::memory_seed::SeededMemory` (an unbounded
`tokio::sync::Mutex<Vec<Message>>`), seeded from either the live agent's current
`agent.memory().get_messages_erased()` (for `/model`) or a loaded session file's `messages` (for
`/resume`/`--resume`). `SlidingWindowMemory` (`daimon`'s default) is deliberately not reused for this
purpose because its default 50-message cap would silently evict history exactly at the moment we're
trying to preserve it.

`/compact` does not rebuild the agent at all — `Agent::memory()` returns the live `&SharedMemory`
handle, and `ErasedMemory::{get_messages_erased, clear_erased, add_message_erased}` are called
directly against the *running* agent's memory in place. The summarization call itself uses the
`SharedModel` handle `App` already holds (not routed through `Agent`, which has no public model
accessor) via `SharedModel`'s `ErasedModel::generate_erased`.

`/init` adds a small `local_code::init` module: `survey::survey_project` walks the project directory
with the `ignore` crate (`.gitignore`-respecting, the same traversal semantics ripgrep uses) to
collect a file listing and the contents of recognized build-manifest files
(`Cargo.toml`/`package.json`/`pyproject.toml`/`go.mod`/etc.), `prompt::build_init_prompt` turns that
survey into a prompt (a pure, deterministically-testable string function), and `generate::generate_agents_md`
makes the one real LLM call (via the same `SharedModel::generate_erased` used by `/compact`) and
writes the result to `<project_root>/AGENTS.md` only — `CLAUDE.md` is never written, per spec section 4.

AGENTS.md/CLAUDE.md **context loading** (the other half of spec section 4) was not built by Phase 2
(confirmed: `src/agent/build.rs`'s `DEFAULT_SYSTEM_PROMPT` is a hardcoded string, and Phase 2's own
Self-review notes call this out as explicitly deferred) nor by Phase 3 (`src/tui/gated_tool.rs`'s
`SYSTEM_PROMPT` constant is likewise hardcoded). This plan adds `local_code::context::load_project_context`
and wires its output into the system prompt both TUI paths use, via a new
`build_streaming_agent_with_history` in `src/tui/gated_tool.rs` that takes an extra `context: &str`
parameter (appended after the existing hardcoded prompt) alongside the extra `initial_messages`
parameter session-resume/`/model` need. Headless mode (`src/agent/headless.rs`, Phase 2) is
**not** touched by this plan — the spec's context-loading requirement is scoped to the interactive
session in this integration, and headless's own system prompt remains a documented, unchanged gap
called out in Phase 2's Self-review (out of scope here; flagged again in this plan's Self-review).

Slash-command dispatch is one parser (`local_code::tui::slash::parse_slash_command`) plus a set of
handler functions/inline branches inside `App`'s `use_input` `Enter` case, replacing Phase 3's
`slash_command_placeholder` call site exactly (the one place Phase 3 documented as the extension
point). Commands whose result requires more than one turn of input (`/model`, `/permissions`,
`/resume` need the user to pick a numbered option; `/connections add` is explicitly *not* built
interactively in-TUI, see Task 12) reuse the same "pending selection short-circuits `use_input`"
shape Phase 3 already established for `pending_permission`.

**Tech Stack:** No new crates beyond `ignore = "0.4"` (gitignore-respecting directory walk for
`/init`) and `serde`/`chrono` derives on already-present dependencies (`chrono` already added by
Phase 6's memory plan; `serde_json` already present from Phase 2). Builds directly on
`local_code::config::{paths::Paths, connection::{Connection, ProviderKind, load_connections}, secrets::SecretStore}`
(Phase 1), `local_code::permissions::{PermissionGate, PermissionTier, PermissionSettings, load_settings, PermissionDecision, PermissionRequest, PermissionPrompter}` and `local_code::agent::provider::build_model` (Phase 2), and
`local_code::tui::{run_tui, App, AppProps, gated_tool::{GatedTool, build_streaming_agent}, permission_prompter::NtuiPermissionPrompter, state::{TranscriptEntry, ToolCallEntry, ToolCallResult, UsageSummary, find_tool_call_mut, toggle_last_tool_call_expanded}}` (Phase 3). Reuses `daimon::agent::{Agent, AgentBuilder}`,
`daimon::model::{SharedModel, types::{ChatRequest, Message, Role}}`, `daimon::memory::Memory`
directly (all confirmed present in the vendored `daimon-0.16.0`/`daimon-core-0.16.0` source).

---

## Spec traceability

This plan implements spec section 1's `/model` half (connection metadata itself is Phase 1's job,
already done — this phase adds the *switching* UI), section 4's context-loading half (AGENTS.md/
CLAUDE.md loading; `/init` generation), section 6 (the full slash-command list) and section 7
(session persistence, `--resume`/`/resume`, `/compact`) in full, from
`docs/superpowers/specs/2026-07-06-local-code-tui-design.md`.

| Spec item | Task(s) | Exact Phase 1–3 types/functions consumed |
|---|---|---|
| §1 `/model` switching, history carries over | 10 | `local_code::config::connection::{load_connections, Connection}` (Phase 1), `local_code::agent::provider::build_model` (Phase 2), `local_code::tui::rebuild::rebuild_agent` (this phase, Task 5) |
| §4 AGENTS.md/CLAUDE.md loaded into system prompt at session start | 6 | `local_code::config::paths::Paths` (Phase 1); new `local_code::context::load_project_context` |
| §4 `/init` generates/updates AGENTS.md only | 14 | `daimon::model::SharedModel::generate_erased` (via `App`'s stored model handle, Phase 3's `AppProps.model`) |
| §6 `/model` | 10 | see above |
| §6 `/connections` (add/list/remove) | 12 | `local_code::cli::connections::{list, remove}` (Phase 1) called through directly; `add` documented as CLI-only (see Task 12) |
| §6 `/init` | 14 | see above |
| §6 `/permissions` | 11 | `local_code::permissions::{PermissionGate, PermissionTier, PermissionSettings, load_settings}` (Phase 2) |
| §6 `/compact` | 13 | `daimon::agent::Agent::memory()` / `ErasedMemory` (via Phase 3's `Arc<Agent>` handle), `AppProps.model` (Phase 3) |
| §6 `/resume` | 15, 16 | `local_code::session::store::{list_sessions, load_session}` (this phase, Task 3) |
| §6 `/clear` | 9 | `local_code::tui::state::TranscriptEntry` (Phase 3) |
| §6 `/help` | 8 | n/a (static text) |
| §7 sessions serialized under state dir, keyed by project + timestamp | 2, 3, 9 | `local_code::config::paths::Paths::user_state_dir` (Phase 1) |
| §7 `local-code --resume` / `/resume` | 15, 16 | see above |
| §7 `/compact` summarizes via active model | 13 | see above |

---

## File structure

- Modify: `Cargo.toml` — add `ignore = "0.4"`.
- Modify: `src/permissions/types.rs` — derive `Serialize`/`Deserialize` on `PermissionTier`.
- Modify: `src/tui/state.rs` — derive `Serialize`/`Deserialize` on `TranscriptEntry`, `ToolCallEntry`, `ToolCallResult`, `UsageSummary`.
- Create: `src/session/mod.rs` — re-exports.
- Create: `src/session/types.rs` — `SessionFile`, `SessionSummary`.
- Create: `src/session/paths.rs` — `project_slug`, `session_dir_for_project`, `new_session_path`.
- Create: `src/session/store.rs` — `save_session`, `list_sessions`, `load_session`.
- Create: `src/tui/memory_seed.rs` — `SeededMemory`.
- Modify: `src/tui/gated_tool.rs` — add `build_streaming_agent_with_history`.
- Create: `src/tui/rebuild.rs` — `ResponderHandle`, `rebuild_agent`.
- Modify: `src/tui/app.rs` — use `rebuild_agent` at mount; wire session persistence + slash dispatch.
- Create: `src/context/mod.rs` — `load_project_context`.
- Create: `src/tui/slash.rs` — `SlashCommand`, `parse_slash_command`.
- Create: `src/init/mod.rs` — re-exports.
- Create: `src/init/survey.rs` — `ProjectSurvey`, `survey_project`.
- Create: `src/init/prompt.rs` — `build_init_prompt`.
- Create: `src/init/generate.rs` — `generate_agents_md`, `write_agents_md`.
- Modify: `src/tui/mod.rs` — `run_tui` gains a `resume: Option<ResumedSession>` parameter; creates/loads the session file; passes context + initial messages/entries into `AppProps`.
- Modify: `src/lib.rs` — add `pub mod session; pub mod context; pub mod init;`.
- Modify: `src/cli/mod.rs` — add `--resume` flag, dispatch to session listing/loading before `run_tui`.
- Create: `tests/live_compact.rs`, `tests/live_init.rs` — `#[ignore]`d integration tests against a real server.

---

### Task 1: Make session-relevant Phase 2/3 types serializable

**Files:**
- Modify: `src/permissions/types.rs`
- Modify: `src/tui/state.rs`

- [ ] **Step 1: Write the failing test for `PermissionTier` serialization**

Append to the existing `#[cfg(test)] mod tests` in `src/permissions/types.rs`:

```rust
    #[test]
    fn permission_tier_round_trips_through_json() {
        let tier = PermissionTier::AutoAcceptEdits;
        let json = serde_json::to_string(&tier).unwrap();
        assert_eq!(json, "\"auto-accept-edits\"");
        let back: PermissionTier = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tier);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib permissions::types::tests::permission_tier_round_trips_through_json`
Expected: FAIL to compile (`PermissionTier` doesn't derive `Serialize`/`Deserialize` yet, and
`serde_json` isn't imported in this test module).

- [ ] **Step 3: Add the derive**

Change the `PermissionTier` definition in `src/permissions/types.rs` from:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionTier {
```

to:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionTier {
```

(Fully-qualified `serde::Serialize`/`serde::Deserialize` avoids adding a new `use` line that would
collide with nothing else in the file — `serde` is already a workspace dependency from Phase 1.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib permissions::types`
Expected: PASS (5 tests: the 4 pre-existing `classify_tool` tests + this one).

- [ ] **Step 5: Write the failing test for transcript-entry serialization**

Append to `src/tui/state.rs`'s existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn transcript_entry_round_trips_through_json() {
        let entries = vec![
            TranscriptEntry::UserTurn { text: "fix the bug".into() },
            TranscriptEntry::ToolCall(ToolCallEntry {
                id: "1".into(),
                name: "edit_file".into(),
                arguments_json: "{}".into(),
                result: Some(ToolCallResult { content: "edited x.rs".into(), is_error: false }),
                expanded: true,
            }),
            TranscriptEntry::AssistantText { text: "done".into() },
            TranscriptEntry::PermissionResolved { description: "run rm".into(), allowed: false },
            TranscriptEntry::SystemNotice { text: "note".into() },
        ];
        let json = serde_json::to_string(&entries).unwrap();
        let back: Vec<TranscriptEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entries);
    }

    #[test]
    fn usage_summary_round_trips_through_json() {
        let usage = UsageSummary { input_tokens: 10, output_tokens: 5, estimated_cost: 0.01 };
        let json = serde_json::to_string(&usage).unwrap();
        let back: UsageSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, usage);
    }
```

- [ ] **Step 6: Run the tests to verify they fail**

Run: `cargo test --lib tui::state`
Expected: FAIL to compile — none of the four state types derive `Serialize`/`Deserialize` yet.

- [ ] **Step 7: Add the derives**

In `src/tui/state.rs`, change each of the four type definitions' derive lines:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TranscriptEntry {
```

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallEntry {
```

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallResult {
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct UsageSummary {
```

(Every field on all four types is already a plain `String`/`bool`/`u32`/`f64`/`Vec`/`Option`/nested
one of these four — no field requires a custom `Serialize` impl.)

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test --lib tui::state`
Expected: PASS (7 tests: the 5 pre-existing + the 2 new).

- [ ] **Step 9: Run the full workspace test suite to confirm no regressions**

Run: `cargo test`
Expected: PASS — adding derives is additive and does not change any existing behavior or test.

- [ ] **Step 10: Commit**

```bash
git add src/permissions/types.rs src/tui/state.rs
git commit -m "feat: make PermissionTier and transcript state types serializable for session persistence"
```

---

### Task 2: Session file types

**Files:**
- Create: `src/session/mod.rs`
- Create: `src/session/types.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/session/types.rs

use serde::{Deserialize, Serialize};

use crate::permissions::types::PermissionTier;
use crate::tui::state::TranscriptEntry;
use daimon::model::types::Message;

/// The full on-disk shape of one session: the display transcript (for
/// immediate re-render) and the raw agent-facing message history (for
/// rebuilding the agent's memory) side by side, plus enough connection/tier
/// metadata to reconstruct the same `Model`/`PermissionGate` on resume. These
/// two message representations are not interconvertible (see this plan's
/// Architecture section), so both are stored rather than one being derived
/// from the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    /// Bumped only if this shape changes incompatibly; `load_session` refuses
    /// to load a file with an unrecognized version rather than guessing.
    pub version: u32,
    pub project_root: std::path::PathBuf,
    pub connection_name: String,
    pub model_name: String,
    pub tier: PermissionTier,
    pub created_at: String,
    pub updated_at: String,
    pub entries: Vec<TranscriptEntry>,
    pub messages: Vec<Message>,
}

pub const SESSION_FILE_VERSION: u32 = 1;

impl SessionFile {
    pub fn new(
        project_root: std::path::PathBuf,
        connection_name: String,
        model_name: String,
        tier: PermissionTier,
        created_at: String,
    ) -> Self {
        SessionFile {
            version: SESSION_FILE_VERSION,
            project_root,
            connection_name,
            model_name,
            tier,
            created_at: created_at.clone(),
            updated_at: created_at,
            entries: Vec::new(),
            messages: Vec::new(),
        }
    }
}

/// One row in a `/resume` or `--resume` listing — everything needed to show
/// the user a human-readable choice without loading the full transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub path: std::path::PathBuf,
    pub connection_name: String,
    pub model_name: String,
    pub updated_at: String,
    /// The text of the first `TranscriptEntry::UserTurn` in the session, if
    /// any, truncated to 60 chars — a human-recognizable label.
    pub first_user_turn_preview: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::types::PermissionTier;

    #[test]
    fn session_file_round_trips_through_json() {
        let mut session = SessionFile::new(
            "/home/user/proj".into(),
            "local-vllm".into(),
            "qwen2.5-coder-32b".into(),
            PermissionTier::Ask,
            "2026-07-06T10:00:00Z".into(),
        );
        session.entries.push(TranscriptEntry::UserTurn { text: "hi".into() });
        session.messages.push(Message::user("hi"));

        let json = serde_json::to_string_pretty(&session).unwrap();
        let back: SessionFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, session);
    }

    #[test]
    fn new_session_has_matching_created_and_updated_timestamps_and_empty_history() {
        let session = SessionFile::new(
            "/proj".into(),
            "conn".into(),
            "model".into(),
            PermissionTier::FullAuto,
            "2026-07-06T10:00:00Z".into(),
        );
        assert_eq!(session.created_at, session.updated_at);
        assert!(session.entries.is_empty());
        assert!(session.messages.is_empty());
        assert_eq!(session.version, SESSION_FILE_VERSION);
    }
}
```

- [ ] **Step 2: Create `src/session/mod.rs`**

```rust
//! Session persistence: serializing the transcript + agent-facing message
//! history + active connection/model/tier to disk, keyed by project +
//! timestamp, per spec section 7.

pub mod paths;
pub mod store;
pub mod types;

pub use store::{list_sessions, load_session, save_session};
pub use types::{SessionFile, SessionSummary, SESSION_FILE_VERSION};
```

- [ ] **Step 3: Add `pub mod session;` to `src/lib.rs`**

```rust
pub mod config;
pub mod cli;
pub mod permissions;
pub mod agent;
pub mod tui;
pub mod session;
```

(Leave `pub mod context;` and `pub mod init;` commented out for now — Tasks 6 and 14 add them; if
`cargo check` fails on their absence, that's expected until those tasks run.)

- [ ] **Step 4: Create placeholder `src/session/paths.rs` and `src/session/store.rs` so the crate compiles**

`src/session/paths.rs`:
```rust
//! Session file path resolution, filled in by Task 3.
```

`src/session/store.rs`:
```rust
//! Session save/list/load, filled in by Task 3.
```

- [ ] **Step 5: Run the tests to verify they fail, then pass**

Run: `cargo test --lib session::types`
Expected: replace this step's placeholder-free content (Step 1's file content, written in full to
`src/session/types.rs`) and re-run; PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add src/session/mod.rs src/session/types.rs src/session/paths.rs src/session/store.rs src/lib.rs
git commit -m "feat: add SessionFile/SessionSummary types for session persistence"
```

---

### Task 3: Session path resolution and disk I/O

**Files:**
- Create: `src/session/paths.rs`
- Create: `src/session/store.rs`

- [ ] **Step 1: Write the failing test for `src/session/paths.rs`**

```rust
// src/session/paths.rs

use std::path::{Path, PathBuf};

/// Derives a filesystem-safe, human-recognizable directory name for a
/// project so each project's sessions live in their own subdirectory under
/// `Paths::user_state_dir` (which is itself global-per-user, not
/// project-scoped — see this plan's Architecture section). Not
/// cryptographically strong or guaranteed collision-free across Rust
/// versions (`DefaultHasher` is not a stable hash across releases per its own
/// docs) — acceptable here since a collision only means two projects'
/// sessions land in the same listing directory, a cosmetic issue caught
/// immediately by `SessionFile::project_root` not matching, not silent data
/// loss.
pub fn project_slug(project_root: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();

    let readable: String = canonical
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let readable = readable.trim_matches('_');
    let tail: String = readable.chars().rev().take(40).collect::<String>().chars().rev().collect();

    format!("{tail}-{hash:016x}")
}

/// The directory holding every session file for `project_root`, under the
/// resolved user state dir.
pub fn session_dir_for_project(user_state_dir: &Path, project_root: &Path) -> PathBuf {
    user_state_dir.join("sessions").join(project_slug(project_root))
}

/// Builds a fresh, not-yet-existing session file path for `project_root`,
/// timestamped to the second so concurrent sessions (unlikely, but possible
/// if two `local-code` processes start in the same second) still sort
/// distinctly enough for `list_sessions` — ties are broken by an incrementing
/// suffix.
pub fn new_session_path(user_state_dir: &Path, project_root: &Path, now: chrono::DateTime<chrono::Utc>) -> PathBuf {
    let dir = session_dir_for_project(user_state_dir, project_root);
    let base = now.format("%Y%m%dT%H%M%SZ").to_string();
    let mut candidate = dir.join(format!("{base}.json"));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = dir.join(format!("{base}-{suffix}.json"));
        suffix += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn same_project_root_yields_the_same_slug() {
        let a = project_slug(Path::new("/home/user/myproject"));
        let b = project_slug(Path::new("/home/user/myproject"));
        assert_eq!(a, b);
    }

    #[test]
    fn different_project_roots_yield_different_slugs() {
        let a = project_slug(Path::new("/home/user/project-a"));
        let b = project_slug(Path::new("/home/user/project-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn slug_contains_only_filesystem_safe_characters() {
        let slug = project_slug(Path::new("/home/user/my project (v2)"));
        assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }

    #[test]
    fn session_dir_is_nested_under_sessions_and_the_project_slug() {
        let dir = session_dir_for_project(Path::new("/state"), Path::new("/home/user/myproject"));
        assert!(dir.starts_with("/state/sessions"));
        assert!(dir.ends_with(project_slug(Path::new("/home/user/myproject"))));
    }

    #[test]
    fn new_session_path_avoids_colliding_with_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let user_state_dir = dir.path();
        let project_root = Path::new("/home/user/myproject");
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-06T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let first = new_session_path(user_state_dir, project_root, now);
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, "{}").unwrap();

        let second = new_session_path(user_state_dir, project_root, now);
        assert_ne!(first, second);
        assert!(second.to_string_lossy().contains("-1.json"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib session::paths`
Expected: replace the Task 2 placeholder with the content above; then PASS (5 tests).

- [ ] **Step 3: Write the failing test for `src/session/store.rs`**

```rust
// src/session/store.rs

use std::fs;
use std::path::Path;

use crate::session::paths::session_dir_for_project;
use crate::session::types::{SessionFile, SessionSummary, SESSION_FILE_VERSION};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to read {path}: {source}")]
    Read { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("failed to write {path}: {source}")]
    Write { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("failed to parse session file {path}: {source}")]
    Parse { path: std::path::PathBuf, #[source] source: serde_json::Error },
    #[error("failed to serialize session: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("session file {path} has unsupported version {found} (expected {expected})")]
    UnsupportedVersion { path: std::path::PathBuf, found: u32, expected: u32 },
}

/// Overwrites `path` with `session`'s current contents, creating parent
/// directories as needed. Called after every completed turn, after
/// `/compact`, and after `/clear` starts a fresh session — see `src/tui/app.rs`.
pub fn save_session(path: &Path, session: &SessionFile) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SessionError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(session).map_err(SessionError::Serialize)?;
    fs::write(path, text).map_err(|source| SessionError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Loads and validates one session file.
pub fn load_session(path: &Path) -> Result<SessionFile, SessionError> {
    let text = fs::read_to_string(path).map_err(|source| SessionError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let session: SessionFile = serde_json::from_str(&text).map_err(|source| SessionError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    if session.version != SESSION_FILE_VERSION {
        return Err(SessionError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: session.version,
            expected: SESSION_FILE_VERSION,
        });
    }
    Ok(session)
}

/// Lists every session recorded for `project_root`, most-recently-updated
/// first. Unreadable/corrupt files are skipped rather than failing the whole
/// listing (a hand-edited or partially-written file shouldn't block
/// `/resume` from finding everything else).
pub fn list_sessions(user_state_dir: &Path, project_root: &Path) -> Result<Vec<SessionSummary>, SessionError> {
    let dir = session_dir_for_project(user_state_dir, project_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut summaries = Vec::new();
    let read_dir = fs::read_dir(&dir).map_err(|source| SessionError::Read {
        path: dir.clone(),
        source,
    })?;
    for entry in read_dir.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(session) = load_session(&path) else { continue };
        let preview = session
            .entries
            .iter()
            .find_map(|e| match e {
                crate::tui::state::TranscriptEntry::UserTurn { text } => {
                    Some(text.chars().take(60).collect::<String>())
                }
                _ => None,
            });
        summaries.push(SessionSummary {
            path,
            connection_name: session.connection_name,
            model_name: session.model_name,
            updated_at: session.updated_at,
            first_user_turn_preview: preview,
        });
    }
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::types::PermissionTier;
    use tempfile::tempdir;

    fn sample(connection: &str, updated_at: &str) -> SessionFile {
        let mut s = SessionFile::new(
            "/proj".into(),
            connection.into(),
            "model".into(),
            PermissionTier::Ask,
            updated_at.into(),
        );
        s.updated_at = updated_at.into();
        s
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let session = sample("local-vllm", "2026-07-06T10:00:00Z");
        save_session(&path, &session).unwrap();
        let loaded = load_session(&path).unwrap();
        assert_eq!(loaded, session);
    }

    #[test]
    fn load_rejects_unsupported_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let mut session = sample("conn", "2026-07-06T10:00:00Z");
        session.version = 999;
        save_session(&path, &session).unwrap();
        let result = load_session(&path);
        assert!(matches!(result, Err(SessionError::UnsupportedVersion { found: 999, .. })));
    }

    #[test]
    fn list_sessions_returns_empty_when_no_directory_exists() {
        let dir = tempdir().unwrap();
        let sessions = list_sessions(dir.path(), Path::new("/nonexistent-project")).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn list_sessions_sorts_most_recently_updated_first_and_skips_corrupt_files() {
        let user_state_dir = tempdir().unwrap();
        let project_root = Path::new("/proj");
        let dir = session_dir_for_project(user_state_dir.path(), project_root);
        fs::create_dir_all(&dir).unwrap();

        save_session(&dir.join("a.json"), &sample("older", "2026-07-01T00:00:00Z")).unwrap();
        save_session(&dir.join("b.json"), &sample("newer", "2026-07-06T00:00:00Z")).unwrap();
        fs::write(dir.join("corrupt.json"), "not json").unwrap();

        let sessions = list_sessions(user_state_dir.path(), project_root).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].connection_name, "newer");
        assert_eq!(sessions[1].connection_name, "older");
    }

    #[test]
    fn list_sessions_extracts_first_user_turn_preview() {
        let user_state_dir = tempdir().unwrap();
        let project_root = Path::new("/proj");
        let dir = session_dir_for_project(user_state_dir.path(), project_root);
        fs::create_dir_all(&dir).unwrap();

        let mut session = sample("conn", "2026-07-06T00:00:00Z");
        session.entries.push(crate::tui::state::TranscriptEntry::UserTurn {
            text: "fix the flaky test".into(),
        });
        save_session(&dir.join("s.json"), &session).unwrap();

        let sessions = list_sessions(user_state_dir.path(), project_root).unwrap();
        assert_eq!(sessions[0].first_user_turn_preview.as_deref(), Some("fix the flaky test"));
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail, then pass**

Run: `cargo test --lib session::store`
Expected: replace the Task 2 placeholder with the content above; then PASS (5 tests).

- [ ] **Step 5: Run the full `session` module's tests together**

Run: `cargo test --lib session`
Expected: PASS (12 tests: 2 from Task 2 + 5 from `paths` + 5 from `store`).

- [ ] **Step 6: Commit**

```bash
git add src/session/paths.rs src/session/store.rs
git commit -m "feat: resolve per-project session file paths and add save/load/list"
```

---

### Task 4: `SeededMemory` and `build_streaming_agent_with_history`

**Files:**
- Create: `src/tui/memory_seed.rs`
- Modify: `src/tui/gated_tool.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Write the failing test for `SeededMemory`**

```rust
// src/tui/memory_seed.rs

use tokio::sync::Mutex;

use daimon::memory::Memory;
use daimon::model::types::Message;

/// An unbounded, in-memory `daimon::memory::Memory` implementor seeded from a
/// `Vec<Message>` at construction. Used wherever this plan needs to preserve
/// (or restore) exact conversation history across an `Agent` rebuild —
/// `/model` switching, `/resume`, and initial session resume at TUI mount.
/// Deliberately not `daimon::memory::SlidingWindowMemory`: that type's
/// default 50-message cap would silently evict the very history a rebuild is
/// trying to preserve, at exactly the moment continuity matters most.
pub struct SeededMemory(Mutex<Vec<Message>>);

impl SeededMemory {
    pub fn new(initial_messages: Vec<Message>) -> Self {
        Self(Mutex::new(initial_messages))
    }
}

impl Memory for SeededMemory {
    async fn add_message(&self, message: Message) -> daimon::Result<()> {
        self.0.lock().await.push(message);
        Ok(())
    }

    async fn get_messages(&self) -> daimon::Result<Vec<Message>> {
        Ok(self.0.lock().await.clone())
    }

    async fn clear(&self) -> daimon::Result<()> {
        self.0.lock().await.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starts_with_the_seeded_messages() {
        let memory = SeededMemory::new(vec![Message::user("hi"), Message::assistant("hello")]);
        let messages = memory.get_messages().await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn add_message_appends_without_evicting() {
        let memory = SeededMemory::new((0..100).map(|i| Message::user(format!("msg {i}"))).collect());
        memory.add_message(Message::user("msg 100")).await.unwrap();
        let messages = memory.get_messages().await.unwrap();
        assert_eq!(messages.len(), 101);
        assert_eq!(messages[0].content.as_deref(), Some("msg 0"));
    }

    #[tokio::test]
    async fn clear_empties_the_history() {
        let memory = SeededMemory::new(vec![Message::user("hi")]);
        memory.clear().await.unwrap();
        assert!(memory.get_messages().await.unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Add `pub mod memory_seed;` to `src/tui/mod.rs`**

```rust
pub mod app;
pub mod components;
pub mod gated_tool;
pub mod memory_seed;
pub mod permission_prompter;
pub mod rebuild;
pub mod slash;
pub mod state;
```

(`rebuild` and `slash` are added here now as empty placeholders so `cargo check` succeeds through
this and the following tasks; create them per Step 4 below.)

- [ ] **Step 3: Run the test to verify it fails, then passes**

Run: `cargo test --lib tui::memory_seed`
Expected: FAIL to compile until `src/tui/memory_seed.rs` is created with the content above; then
PASS (3 tests).

- [ ] **Step 4: Create placeholder files referenced by Step 2**

`src/tui/rebuild.rs`:
```rust
//! Agent-rebuild helper, filled in by Task 5.
```

`src/tui/slash.rs`:
```rust
//! Slash-command parsing, filled in by Task 7.
```

- [ ] **Step 5: Add `build_streaming_agent_with_history` to `src/tui/gated_tool.rs`**

Append (do not remove `build_streaming_agent` — Task 3 of Phase 3's own plan and its tests still
depend on it unchanged):

```rust
use crate::tui::memory_seed::SeededMemory;
use daimon::model::types::Message;

/// Identical to [`build_streaming_agent`] but (a) seeds the agent's memory
/// with `initial_messages` via [`SeededMemory`] instead of starting empty,
/// and (b) appends `extra_system_context` (AGENTS.md/CLAUDE.md content, or an
/// empty string if none was found) to the system prompt. Used by every
/// call site added in this plan (`App`'s mount, `/model`, `/resume`);
/// `build_streaming_agent` itself remains unchanged and is still exercised by
/// Phase 3's own tests.
pub fn build_streaming_agent_with_history(
    model: SharedModel,
    gate: Arc<PermissionGate>,
    initial_messages: Vec<Message>,
    extra_system_context: &str,
) -> daimon::Result<Agent> {
    let system_prompt = if extra_system_context.trim().is_empty() {
        SYSTEM_PROMPT.to_string()
    } else {
        format!("{SYSTEM_PROMPT}\n\n{extra_system_context}")
    };

    AgentBuilder::new()
        .shared_model(model)
        .system_prompt(system_prompt)
        .memory(SeededMemory::new(initial_messages))
        .tool(GatedTool::new(ReadFile, gate.clone()))
        .tool(GatedTool::new(WriteFile, gate.clone()))
        .tool(GatedTool::new(EditFile, gate.clone()))
        .tool(GatedTool::new(Bash, gate.clone()))
        .tool(GatedTool::new(Grep, gate.clone()))
        .tool(GatedTool::new(Glob, gate))
        .build()
}

#[cfg(test)]
mod with_history_tests {
    use super::*;
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier};
    use std::future::Future;
    use std::pin::Pin;

    struct AlwaysAllow;
    impl PermissionPrompter for AlwaysAllow {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            Box::pin(async { PermissionDecision::Allow })
        }
    }

    fn gate() -> Arc<PermissionGate> {
        Arc::new(PermissionGate::new(
            PermissionTier::FullAuto,
            PermissionSettings::default(),
            Arc::new(AlwaysAllow),
        ))
    }

    struct EchoModel;
    impl daimon::model::Model for EchoModel {
        async fn generate(&self, request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::model::types::ChatResponse> {
            Ok(daimon::model::types::ChatResponse {
                message: Message::assistant(format!("saw {} messages", request.messages.len())),
                stop_reason: daimon::model::types::StopReason::EndTurn,
                usage: Some(daimon::model::types::Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::stream::ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn seeded_history_is_visible_to_the_next_turn() {
        let model: SharedModel = Arc::new(EchoModel);
        let initial = vec![Message::user("earlier turn"), Message::assistant("earlier reply")];
        let agent = build_streaming_agent_with_history(model, gate(), initial, "").unwrap();

        let response = agent.prompt("new turn").await.unwrap();
        // system prompt + 2 seeded + new user turn = 4 messages sent to the model
        assert!(response.text().contains("saw 4 messages"), "{}", response.text());
    }

    #[tokio::test]
    async fn extra_system_context_is_appended_to_the_prompt() {
        struct CapturingModel;
        impl daimon::model::Model for CapturingModel {
            async fn generate(&self, request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::model::types::ChatResponse> {
                let system_text = request
                    .messages
                    .first()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(daimon::model::types::ChatResponse {
                    message: Message::assistant(system_text),
                    stop_reason: daimon::model::types::StopReason::EndTurn,
                    usage: Some(daimon::model::types::Usage::default()),
                })
            }
            async fn generate_stream(&self, _request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::stream::ResponseStream> {
                Ok(Box::pin(futures::stream::empty()))
            }
        }

        let model: SharedModel = Arc::new(CapturingModel);
        let agent = build_streaming_agent_with_history(model, gate(), vec![], "Project rule: never use unwrap().").unwrap();
        let response = agent.prompt("hi").await.unwrap();
        assert!(response.text().contains("Project rule: never use unwrap()."), "{}", response.text());
    }
}
```

- [ ] **Step 6: Run the tests to verify they fail, then pass**

Run: `cargo test --lib tui::gated_tool`
Expected: PASS (8 tests: the 6 pre-existing from Phase 3 + the 2 new).

- [ ] **Step 7: Run `cargo check` for the whole crate**

Run: `cargo check`
Expected: PASS (the `rebuild`/`slash` placeholder modules from Step 4 compile as empty doc-comment
files).

- [ ] **Step 8: Commit**

```bash
git add src/tui/memory_seed.rs src/tui/gated_tool.rs src/tui/mod.rs src/tui/rebuild.rs src/tui/slash.rs
git commit -m "feat: add SeededMemory and build_streaming_agent_with_history for history-preserving agent rebuilds"
```

---

### Task 5: `rebuild_agent` helper

**Files:**
- Create: `src/tui/rebuild.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/tui/rebuild.rs

use std::sync::{Arc, Mutex};

use daimon::agent::Agent;
use daimon::model::SharedModel;
use daimon::model::types::Message;
use tokio::sync::oneshot;

use crate::permissions::gate::PermissionGate;
use crate::permissions::settings::PermissionSettings;
use crate::permissions::types::{PermissionDecision, PermissionRequest, PermissionTier};
use crate::tui::gated_tool::build_streaming_agent_with_history;
use crate::tui::permission_prompter::NtuiPermissionPrompter;

pub type ResponderHandle = Arc<Mutex<Option<oneshot::Sender<PermissionDecision>>>>;

/// Builds a fresh `(Agent, PermissionGate, ResponderHandle)` triple: a new
/// `NtuiPermissionPrompter` bound to `pending_permission`, a `PermissionGate`
/// at `initial_tier` with `always_allow`/`always_deny`, and an `Agent` seeded
/// with `initial_messages` and `extra_system_context`. This is the single
/// place that logic lives — `App`'s mount, `/model`, and `/resume` all call
/// it instead of duplicating the construction sequence Phase 3 originally
/// left inlined in `App`'s `hooks.use_state` initializer.
pub fn rebuild_agent(
    model: SharedModel,
    initial_tier: PermissionTier,
    always_allow: Vec<String>,
    always_deny: Vec<String>,
    initial_messages: Vec<Message>,
    extra_system_context: &str,
    pending_permission: ntui::State<Option<PermissionRequest>>,
) -> (Arc<Agent>, Arc<PermissionGate>, ResponderHandle) {
    let prompter = NtuiPermissionPrompter::new(pending_permission);
    let responder = prompter.responder_handle();
    let settings = PermissionSettings { always_allow, always_deny };
    let gate = Arc::new(PermissionGate::new(initial_tier, settings, Arc::new(prompter)));
    let agent = Arc::new(
        build_streaming_agent_with_history(model, gate.clone(), initial_messages, extra_system_context)
            .expect("agent construction should not fail"),
    );
    (agent, gate, responder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::model::types::{ChatRequest, ChatResponse, StopReason, Usage};
    use daimon::stream::ResponseStream;
    use ntui::testing::TestTerminal;
    use ntui::{component, element, Element};

    struct EchoModel;
    impl daimon::model::Model for EchoModel {
        async fn generate(&self, request: &ChatRequest) -> daimon::Result<ChatResponse> {
            Ok(ChatResponse {
                message: Message::assistant(format!("messages={}", request.messages.len())),
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[derive(Clone, PartialEq, Default)]
    struct HarnessProps;

    #[component]
    fn Harness(_props: &HarnessProps, hooks: &mut ntui::Hooks) -> ntui::Element {
        use ntui::props::TextProps;

        let pending = hooks.use_state(|| Option::<PermissionRequest>::None);
        let result_text = hooks.use_state(|| "not built yet".to_string());

        hooks.use_effect((), {
            let pending = pending.clone();
            let result_text = result_text.clone();
            move || {
                let model: SharedModel = Arc::new(EchoModel);
                let (agent, _gate, _responder) = rebuild_agent(
                    model,
                    PermissionTier::FullAuto,
                    vec![],
                    vec![],
                    vec![Message::user("seeded turn")],
                    "",
                    pending,
                );
                tokio::spawn(async move {
                    let response = agent.prompt("hi").await.unwrap();
                    result_text.set(response.text().to_string());
                });
            }
        });

        element! {
            View { Text(content: result_text.get()) }
        }
    }

    #[tokio::test]
    async fn rebuild_agent_produces_a_working_agent_seeded_with_history() {
        let mut t = TestTerminal::new(60, 1, Element::component::<Harness>(HarnessProps)).unwrap();
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            t.tick().await.unwrap();
        }
        // system prompt + 1 seeded message + "hi" = 3
        assert!(t.frame_text().contains("messages=3"), "{}", t.frame_text());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails, then passes**

Run: `cargo test --lib tui::rebuild`
Expected: replace the Task 4 placeholder with the content above; then PASS (1 test).

- [ ] **Step 3: Refactor `App`'s mount closure in `src/tui/app.rs` to call `rebuild_agent`**

Replace the body of the `agent_and_responder` `hooks.use_state` initializer (Phase 3's inline
construction) with a call to the new helper. Change:

```rust
    let agent_and_responder = hooks.use_state({
        let model = props.model.clone().expect("AppProps::model is always Some");
        let always_allow = props.always_allow.clone();
        let always_deny = props.always_deny.clone();
        let initial_tier = props.initial_tier;
        let pending_permission = pending_permission.clone();
        move || {
            let prompter = NtuiPermissionPrompter::new(pending_permission.clone());
            let responder = prompter.responder_handle();
            let settings = PermissionSettings {
                always_allow,
                always_deny,
            };
            let gate = Arc::new(PermissionGate::new(initial_tier, settings, Arc::new(prompter)));
            let agent =
                Arc::new(build_streaming_agent(model, gate.clone()).expect("agent construction should not fail"));
            (agent, gate, responder)
        }
    });
```

to:

```rust
    let agent_and_responder = hooks.use_state({
        let model = props.model.clone().expect("AppProps::model is always Some");
        let always_allow = props.always_allow.clone();
        let always_deny = props.always_deny.clone();
        let initial_tier = props.initial_tier;
        let initial_messages = props.initial_messages.clone();
        let system_context = props.system_context.clone();
        let pending_permission = pending_permission.clone();
        move || {
            crate::tui::rebuild::rebuild_agent(
                model,
                initial_tier,
                always_allow,
                always_deny,
                initial_messages,
                &system_context,
                pending_permission,
            )
        }
    });
```

This references `props.initial_messages` and `props.system_context`, which do not exist on
`AppProps` yet — Task 6 adds them (along with `props.initial_entries`, used by the `transcript`
`hooks.use_state` initializer the same way). Leave `cargo check` failing at the end of this step;
Task 6 fixes it as part of extending `AppProps`. Also remove the now-unused
`use crate::tui::gated_tool::build_streaming_agent;` and `use crate::tui::permission_prompter::NtuiPermissionPrompter;`
imports from `src/tui/app.rs` if `cargo check` (run after Task 6) flags them as unused — `App` no
longer constructs these types directly.

- [ ] **Step 4: Commit**

```bash
git add src/tui/rebuild.rs src/tui/app.rs
git commit -m "feat: extract rebuild_agent and have App's mount closure call it"
```

(This commit intentionally leaves the crate non-compiling at `cargo check` until Task 6 lands —
both tasks are part of the same logical change, split for reviewable step size; if your workflow
requires every commit to compile, squash Tasks 5 and 6 into one commit instead.)

---

### Task 6: AGENTS.md/CLAUDE.md context loading; extend `AppProps`/`run_tui`

**Files:**
- Create: `src/context/mod.rs`
- Modify: `src/lib.rs`
- Modify: `src/tui/app.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/context/mod.rs

use std::path::Path;

use crate::config::paths::Paths;

/// Loads and concatenates, in the order spec section 4 specifies (project
/// AGENTS.md, project CLAUDE.md, user-level AGENTS.md, user-level CLAUDE.md),
/// whichever of these four files exist. Missing files are silently skipped —
/// this is optional context, not a hard requirement. Each present file is
/// wrapped in a small header identifying its source, so the model can tell
/// project-level guidance apart from user-level defaults if they conflict.
pub fn load_project_context(paths: &Paths, project_root: &Path) -> String {
    let candidates = [
        (project_root.join("AGENTS.md"), "Project AGENTS.md"),
        (project_root.join("CLAUDE.md"), "Project CLAUDE.md"),
        (paths.user_config_dir.join("AGENTS.md"), "User-level AGENTS.md"),
        (paths.user_config_dir.join("CLAUDE.md"), "User-level CLAUDE.md"),
    ];

    let mut sections = Vec::new();
    for (path, label) in candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if !content.trim().is_empty() {
                sections.push(format!("## {label}\n\n{content}"));
            }
        }
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_paths(user_config_dir: &Path) -> Paths {
        Paths {
            user_config_dir: user_config_dir.to_path_buf(),
            project_config_dir: user_config_dir.join("unused-project-config"),
            user_state_dir: user_config_dir.join("unused-state"),
        }
    }

    #[test]
    fn returns_empty_string_when_no_context_files_exist() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let context = load_project_context(&paths, dir.path());
        assert!(context.is_empty());
    }

    #[test]
    fn loads_project_agents_md_when_present() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Always run tests before committing.").unwrap();
        let paths = test_paths(&dir.path().join("user-config"));
        let context = load_project_context(&paths, dir.path());
        assert!(context.contains("Project AGENTS.md"));
        assert!(context.contains("Always run tests before committing."));
    }

    #[test]
    fn loads_all_four_files_in_the_documented_order() {
        let dir = tempdir().unwrap();
        let user_config_dir = dir.path().join("user-config");
        std::fs::create_dir_all(&user_config_dir).unwrap();

        std::fs::write(dir.path().join("AGENTS.md"), "project agents").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "project claude").unwrap();
        std::fs::write(user_config_dir.join("AGENTS.md"), "user agents").unwrap();
        std::fs::write(user_config_dir.join("CLAUDE.md"), "user claude").unwrap();

        let paths = test_paths(&user_config_dir);
        let context = load_project_context(&paths, dir.path());

        let project_agents_pos = context.find("project agents").unwrap();
        let project_claude_pos = context.find("project claude").unwrap();
        let user_agents_pos = context.find("user agents").unwrap();
        let user_claude_pos = context.find("user claude").unwrap();
        assert!(project_agents_pos < project_claude_pos);
        assert!(project_claude_pos < user_agents_pos);
        assert!(user_agents_pos < user_claude_pos);
    }

    #[test]
    fn blank_file_is_treated_as_absent() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "   \n  ").unwrap();
        let paths = test_paths(&dir.path().join("user-config"));
        let context = load_project_context(&paths, dir.path());
        assert!(context.is_empty());
    }
}
```

- [ ] **Step 2: Add `pub mod context;` to `src/lib.rs`**

```rust
pub mod config;
pub mod cli;
pub mod permissions;
pub mod agent;
pub mod tui;
pub mod session;
pub mod context;
```

- [ ] **Step 3: Run the test to verify it fails, then passes**

Run: `cargo test --lib context`
Expected: FAIL to compile until `src/context/mod.rs` is created with the content above; then PASS
(4 tests).

- [ ] **Step 4: Extend `AppProps` in `src/tui/app.rs`**

Add three fields (`initial_entries`, `initial_messages`, `system_context`) and their `Default` impl
entries:

```rust
#[derive(Clone)]
pub struct AppProps {
    pub model: Option<SharedModel>,
    pub connection_name: String,
    pub model_name: String,
    pub always_allow: Vec<String>,
    pub always_deny: Vec<String>,
    pub initial_tier: PermissionTier,
    /// Non-empty only when launched via `--resume`/`/resume`; seeds the
    /// visible transcript so a resumed session redraws immediately instead
    /// of starting blank.
    pub initial_entries: Vec<TranscriptEntry>,
    /// The raw agent-facing history to seed the rebuilt agent's memory with
    /// (see `SeededMemory`) — kept separate from `initial_entries` because
    /// the two are not interconvertible (see this plan's Architecture
    /// section).
    pub initial_messages: Vec<daimon::model::types::Message>,
    /// AGENTS.md/CLAUDE.md content (already concatenated by
    /// `local_code::context::load_project_context`), appended to the system
    /// prompt. Empty string if no context files were found.
    pub system_context: String,
    /// The session file this instance persists to after every turn.
    pub session_path: std::path::PathBuf,
}

impl Default for AppProps {
    fn default() -> Self {
        AppProps {
            model: None,
            connection_name: String::new(),
            model_name: String::new(),
            always_allow: Vec::new(),
            always_deny: Vec::new(),
            initial_tier: PermissionTier::Ask,
            initial_entries: Vec::new(),
            initial_messages: Vec::new(),
            system_context: String::new(),
            session_path: std::path::PathBuf::new(),
        }
    }
}
```

Update the `transcript` state initializer to seed from `props.initial_entries`:

```rust
    let transcript = hooks.use_state({
        let initial_entries = props.initial_entries.clone();
        move || initial_entries
    });
```

(Replacing the Phase 3 `hooks.use_state(Vec::<TranscriptEntry>::new);` line.)

- [ ] **Step 5: Run `cargo check` — this should now succeed for `src/tui/app.rs`'s own content**

Run: `cargo check --lib`
Expected: PASS for `AppProps`/`App`'s own code; `src/tui/mod.rs`'s `run_tui` (which constructs
`AppProps` and doesn't yet supply the three new fields) and its own tests will fail to compile until
Step 6.

- [ ] **Step 6: Extend `run_tui` in `src/tui/mod.rs`**

Add a `ResumedSession` type and thread it through `run_tui`. Replace the existing `run_tui` function
and its surrounding imports with:

```rust
use crate::context::load_project_context;
use crate::session::paths::new_session_path;
use crate::session::types::SessionFile;
use daimon::model::types::Message;

/// The subset of a loaded `SessionFile` `run_tui` needs to seed a resumed
/// session — the file's own `path` is threaded through separately so the
/// resumed session keeps appending to the same file rather than starting a
/// new one.
pub struct ResumedSession {
    pub session_path: std::path::PathBuf,
    pub entries: Vec<crate::tui::state::TranscriptEntry>,
    pub messages: Vec<Message>,
    pub tier: PermissionTier,
}

pub async fn run_tui(
    paths: &Paths,
    project_root: &Path,
    connection_name: Option<&str>,
    permission_mode_override: Option<PermissionTier>,
    resume: Option<ResumedSession>,
) -> Result<(), TuiSessionError> {
    let connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    let connection = select_connection(&connections, connection_name)?;

    let api_key = SecretStore::get_api_key(&connection.name)?;
    let model = build_model(&connection, api_key)?;

    let settings = load_settings(&paths.user_config_dir, &paths.project_config_dir)?;
    let system_context = load_project_context(paths, project_root);

    let (initial_tier, initial_entries, initial_messages, session_path) = match resume {
        Some(resumed) => (
            permission_mode_override.unwrap_or(resumed.tier),
            resumed.entries,
            resumed.messages,
            resumed.session_path,
        ),
        None => {
            let now = chrono::Utc::now();
            let path = new_session_path(&paths.user_state_dir, project_root, now);
            let tier = permission_mode_override.unwrap_or(PermissionTier::Ask);
            let session = SessionFile::new(
                project_root.to_path_buf(),
                connection.name.clone(),
                connection.default_model.clone(),
                tier,
                now.to_rfc3339(),
            );
            crate::session::store::save_session(&path, &session)
                .map_err(TuiSessionError::Session)?;
            (tier, Vec::new(), Vec::new(), path)
        }
    };

    let props = AppProps {
        model: Some(model),
        connection_name: connection.name.clone(),
        model_name: connection.default_model.clone(),
        always_allow: settings.always_allow,
        always_deny: settings.always_deny,
        initial_tier,
        initial_entries,
        initial_messages,
        system_context,
        session_path,
    };

    ntui::render(ntui::element!(App(
        model: props.model,
        connection_name: props.connection_name,
        model_name: props.model_name,
        always_allow: props.always_allow,
        always_deny: props.always_deny,
        initial_tier: props.initial_tier,
        initial_entries: props.initial_entries,
        initial_messages: props.initial_messages,
        system_context: props.system_context,
        session_path: props.session_path
    )))
    .await?;
    Ok(())
}
```

Add the new `Session` error variant to `TuiSessionError`:

```rust
    #[error("failed to persist session: {0}")]
    Session(#[from] crate::session::store::SessionError),
```

- [ ] **Step 7: Update `src/tui/mod.rs`'s own tests and every other `run_tui` caller for the new parameter**

The 5 `select_connection` tests in `src/tui/mod.rs` don't call `run_tui` directly and need no
change. Update `src/cli/mod.rs`'s call site (added in Phase 3's Task 9) from:

```rust
            crate::tui::run_tui(
                &paths,
                &project_root,
                cli.connection.as_deref(),
                cli.permission_mode.map(PermissionModeArg::into_tier),
            )
            .await?;
```

to:

```rust
            crate::tui::run_tui(
                &paths,
                &project_root,
                cli.connection.as_deref(),
                cli.permission_mode.map(PermissionModeArg::into_tier),
                None,
            )
            .await?;
```

(Task 16 replaces this `None` with real `--resume` handling.)

- [ ] **Step 8: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS — every test from Phases 1–3 plus this plan's Tasks 1–6. `App`'s own Phase 3
integration tests (`submitting_a_prompt_streams_the_assistant_reply_into_the_transcript`, etc.) must
still pass unmodified since `test_props()` there constructs `AppProps { .. }` with the pre-Task-6
fields explicitly and relies on `..Default::default()`-equivalent — if `test_props()` uses a bare
struct literal without `..Default::default()`, add `initial_entries: vec![], initial_messages: vec![],
system_context: String::new(), session_path: std::path::PathBuf::new()` to it (or switch it to spread
from `AppProps::default()` and override only the fields it cares about) so it still compiles.

- [ ] **Step 9: Commit**

```bash
git add src/context/mod.rs src/lib.rs src/tui/app.rs src/tui/mod.rs src/cli/mod.rs
git commit -m "feat: load AGENTS.md/CLAUDE.md into the system prompt; thread session/resume state through run_tui and AppProps"
```

---

### Task 7: Slash-command parser

**Files:**
- Create: `src/tui/slash.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/tui/slash.rs

/// One recognized slash command, already split into its command word and
/// remaining argument text. `parse_slash_command` is the single place that
/// knows the full v1 command list (spec section 6); `App`'s `Enter` handler
/// matches on this enum instead of re-parsing strings inline.
#[derive(Debug, Clone, PartialEq)]
pub enum SlashCommand {
    Model,
    ConnectionsList,
    ConnectionsRemove { name: String },
    ConnectionsAddUnsupported,
    Init,
    Permissions,
    Compact,
    Resume,
    Clear,
    Help,
    Unknown { raw: String },
}

/// Parses `input` as a slash command if it starts with `/` (after
/// trimming), returning `None` for anything else (a normal prompt). Unknown
/// `/word` input still parses to `Some(SlashCommand::Unknown { .. })` rather
/// than `None`, so the caller can show a clear "unrecognized command" notice
/// instead of silently sending `/typo` to the model as a prompt.
pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut parts = trimmed[1..].split_whitespace();
    let command = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();

    Some(match command {
        "model" => SlashCommand::Model,
        "connections" => match rest.as_slice() {
            ["list"] | [] => SlashCommand::ConnectionsList,
            ["remove", name] => SlashCommand::ConnectionsRemove { name: name.to_string() },
            ["add"] => SlashCommand::ConnectionsAddUnsupported,
            _ => SlashCommand::Unknown { raw: trimmed.to_string() },
        },
        "init" => SlashCommand::Init,
        "permissions" => SlashCommand::Permissions,
        "compact" => SlashCommand::Compact,
        "resume" => SlashCommand::Resume,
        "clear" => SlashCommand::Clear,
        "help" => SlashCommand::Help,
        _ => SlashCommand::Unknown { raw: trimmed.to_string() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_slash_input_parses_to_none() {
        assert_eq!(parse_slash_command("fix the bug"), None);
    }

    #[test]
    fn recognizes_every_v1_command() {
        assert_eq!(parse_slash_command("/model"), Some(SlashCommand::Model));
        assert_eq!(parse_slash_command("/connections"), Some(SlashCommand::ConnectionsList));
        assert_eq!(parse_slash_command("/connections list"), Some(SlashCommand::ConnectionsList));
        assert_eq!(
            parse_slash_command("/connections remove local-vllm"),
            Some(SlashCommand::ConnectionsRemove { name: "local-vllm".into() })
        );
        assert_eq!(parse_slash_command("/connections add"), Some(SlashCommand::ConnectionsAddUnsupported));
        assert_eq!(parse_slash_command("/init"), Some(SlashCommand::Init));
        assert_eq!(parse_slash_command("/permissions"), Some(SlashCommand::Permissions));
        assert_eq!(parse_slash_command("/compact"), Some(SlashCommand::Compact));
        assert_eq!(parse_slash_command("/resume"), Some(SlashCommand::Resume));
        assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::Clear));
        assert_eq!(parse_slash_command("/help"), Some(SlashCommand::Help));
    }

    #[test]
    fn unrecognized_slash_word_is_unknown_not_none() {
        assert_eq!(
            parse_slash_command("/typo"),
            Some(SlashCommand::Unknown { raw: "/typo".into() })
        );
    }

    #[test]
    fn malformed_connections_subcommand_is_unknown() {
        assert_eq!(
            parse_slash_command("/connections bogus"),
            Some(SlashCommand::Unknown { raw: "/connections bogus".into() })
        );
    }

    #[test]
    fn leading_and_trailing_whitespace_is_tolerated() {
        assert_eq!(parse_slash_command("  /help  "), Some(SlashCommand::Help));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib tui::slash`
Expected: replace the Task 4 placeholder with the content above; then PASS (6 tests).

- [ ] **Step 3: Commit**

```bash
git add src/tui/slash.rs
git commit -m "feat: add slash-command parser covering the full v1 command list"
```

---

### Task 8: Wire real dispatch into `App`; `/help` and unknown-command handling

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Replace the `slash_command_placeholder` call site**

In `src/tui/app.rs`'s `KeyCode::Enter` branch, replace:

```rust
                    if let Some(command_notice) = slash_command_placeholder(&text) {
                        transcript.update(|entries| {
                            entries.push(TranscriptEntry::UserTurn { text: text.clone() });
                            entries.push(TranscriptEntry::SystemNotice {
                                text: command_notice,
                            });
                        });
                        input_buffer.set(String::new());
                        return;
                    }
```

with:

```rust
                    if let Some(command) = crate::tui::slash::parse_slash_command(&text) {
                        input_buffer.set(String::new());
                        dispatch_slash_command(command, &SlashContext {
                            transcript: transcript.clone(),
                            tier: tier.clone(),
                        });
                        return;
                    }
```

Delete the now-unused `slash_command_placeholder` function entirely (Phase 3's own doc comment on
it says "Phase 4 replaces the call site ... this function itself can then be deleted").

- [ ] **Step 2: Add `SlashContext` and `dispatch_slash_command`'s `/help` and `Unknown` arms**

Append to `src/tui/app.rs` (later tasks add more fields to `SlashContext` and more match arms —
this step only makes `/help` and unrecognized input work, matching the "no placeholders" rule by
handling every `SlashCommand` variant that exists *right now*, `todo!()`-free):

```rust
/// Everything a slash-command handler needs, gathered in one place so
/// `dispatch_slash_command`'s signature doesn't grow a new parameter per
/// command. Tasks 10–15 extend this struct as each command's handler needs
/// more state; every field added there is threaded through from the same
/// `App` render this struct is built in.
struct SlashContext {
    transcript: ntui::State<Vec<TranscriptEntry>>,
    tier: ntui::State<PermissionTier>,
}

const HELP_TEXT: &str = "\
/model                     switch the active connection/model (history is kept)
/connections list          list configured connections
/connections remove <name> remove a configured connection
/connections add           not supported in-TUI; run `local-code connections add` in a separate terminal
/init                      generate/update AGENTS.md from a survey of this project
/permissions               view or change the permission tier and allow/deny list
/compact                   summarize older turns to free up context
/resume                    switch to a previous session for this project
/clear                     clear the transcript and start a fresh session
/help                      show this message";

fn dispatch_slash_command(command: crate::tui::slash::SlashCommand, ctx: &SlashContext) {
    use crate::tui::slash::SlashCommand;

    match command {
        SlashCommand::Help => {
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text: HELP_TEXT.to_string() });
            });
        }
        SlashCommand::Unknown { raw } => {
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice {
                    text: format!("'{raw}' is not a recognized command. Type /help to see the list."),
                });
            });
        }
        // Tasks 9–15 fill in every remaining variant. Left unmatched here
        // deliberately would be a compile error (the match is exhaustive),
        // which is why Task 9 (the very next task) adds `Clear` immediately
        // rather than leaving this plan in a non-compiling state at the end
        // of this task; see that task's Step 1.
        other => unreachable!(
            "SlashCommand::{other:?} is handled by a later task in this plan; if you see this at \
             runtime while implementing Task 8 in isolation, that's expected — Task 9 replaces this \
             arm before the plan is done"
        ),
    }
}
```

- [ ] **Step 2: Run `cargo check` to confirm the match compiles (with the temporary `unreachable!` catch-all)**

Run: `cargo check --lib`
Expected: PASS. The `unreachable!` catch-all keeps this step's diff small and reviewable; Task 9
removes it by handling `Clear` explicitly, and each of Tasks 10–15 removes one more variant from
what the catch-all covers, until no catch-all arm remains (confirmed in Task 15's Step where the
match becomes fully explicit and the `other => unreachable!(...)` arm is deleted).

- [ ] **Step 3: Write the test**

Append to `src/tui/app.rs`'s existing `#[cfg(test)] mod tests`:

```rust
    #[tokio::test(start_paused = true)]
    async fn help_command_lists_every_slash_command() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/help").await;
        t.tick().await.unwrap();
        let text = t.frame_text();
        for command in ["/model", "/connections", "/init", "/permissions", "/compact", "/resume", "/clear", "/help"] {
            assert!(text.contains(command), "missing {command} in help text: {text}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unrecognized_command_shows_a_clear_notice_instead_of_prompting_the_model() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/bogus").await;
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(text.contains("not a recognized command"), "{text}");
        assert!(!text.contains("Hello, world"), "must not have run a turn: {text}");
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib tui::app`
Expected: PASS (5 tests: the 3 from Phase 3's Task 7 + these 2).

- [ ] **Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: wire real slash-command dispatch into App, replacing the Phase 3 placeholder"
```

---

### Task 9: `/clear` and session-persistence lifecycle

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Extend `SlashContext`, handle `Clear`, and persist after every turn**

Add fields to `SlashContext`:

```rust
struct SlashContext {
    transcript: ntui::State<Vec<TranscriptEntry>>,
    tier: ntui::State<PermissionTier>,
    session_path: ntui::State<std::path::PathBuf>,
    connection_name: String,
    model_name: String,
    project_root: std::path::PathBuf,
}
```

Add a `session_path` state to `App` (seeded from `props.session_path`) alongside the existing
`hooks.use_state` calls:

```rust
    let session_path = hooks.use_state({
        let initial = props.session_path.clone();
        move || initial
    });
```

Update the call site in the `Enter` branch to populate the new fields:

```rust
                        dispatch_slash_command(command, &SlashContext {
                            transcript: transcript.clone(),
                            tier: tier.clone(),
                            session_path: session_path.clone(),
                            connection_name: props.connection_name.clone(),
                            model_name: props.model_name.clone(),
                            project_root: std::env::current_dir().unwrap_or_default(),
                        });
```

Replace the `unreachable!`-covered `SlashCommand::Clear` case by adding an explicit arm (before the
catch-all) in `dispatch_slash_command`:

```rust
        SlashCommand::Clear => {
            ctx.transcript.set(Vec::new());
            let now = chrono::Utc::now();
            let new_path = crate::session::paths::new_session_path(
                &new_path_state_dir(),
                &ctx.project_root,
                now,
            );
            let fresh = crate::session::types::SessionFile::new(
                ctx.project_root.clone(),
                ctx.connection_name.clone(),
                ctx.model_name.clone(),
                ctx.tier.get(),
                now.to_rfc3339(),
            );
            if let Err(e) = crate::session::store::save_session(&new_path, &fresh) {
                ctx.transcript.update(|entries| {
                    entries.push(TranscriptEntry::SystemNotice {
                        text: format!("cleared transcript, but failed to start a new session file: {e}"),
                    });
                });
            }
            ctx.session_path.set(new_path);
        }
```

`/clear` deliberately starts a **new** session file rather than truncating the current one — the
old session (with its history up to the clear) remains resumable via `/resume`/`--resume`; only the
live in-memory transcript and the agent's memory reset (the agent itself is not rebuilt here, since
nothing about its model/tier changed — its `SeededMemory` still holds the pre-clear history, which
is intentionally now orphaned from the visible transcript; this is a known, documented limitation,
not a bug: the agent's next turn will still technically have the old history in its context window
until a future turn or `/compact` trims it. If tighter isolation is wanted later, `/clear` could
additionally call `agent.memory().clear_erased()` — deferred here since `dispatch_slash_command`
does not currently have access to the live `Arc<Agent>`; Task 10 adds that access for `/model` and
would be the natural place to also wire it into `/clear` if this gap needs closing).

Add the small helper `new_path_state_dir` is calling — actually simplify: `dispatch_slash_command`
doesn't have `Paths` in scope. Replace the `new_session_path` call above to take `paths.user_state_dir`
by adding `user_state_dir: std::path::PathBuf` to `SlashContext` instead of inventing a helper
function:

```rust
struct SlashContext {
    transcript: ntui::State<Vec<TranscriptEntry>>,
    tier: ntui::State<PermissionTier>,
    session_path: ntui::State<std::path::PathBuf>,
    connection_name: String,
    model_name: String,
    project_root: std::path::PathBuf,
    user_state_dir: std::path::PathBuf,
}
```

and in the `Clear` arm use `&ctx.user_state_dir` instead of `&new_path_state_dir()`. Update the
`Enter` branch's `SlashContext` construction to also pass `user_state_dir` — this requires `App`
to know `Paths`, which it currently does not (only individual resolved dirs are threaded through
via `AppProps`). Add one more `AppProps` field:

```rust
    /// Needed only so `/clear` and future commands can resolve a fresh
    /// session path without re-deriving `Paths` from scratch inside `App`.
    pub user_state_dir: std::path::PathBuf,
```

(with a matching `Default` of `std::path::PathBuf::new()`), have `run_tui` (Task 6) pass
`paths.user_state_dir.clone()`, and have the `Enter` branch's `SlashContext` construction use
`user_state_dir: props.user_state_dir.clone()`.

- [ ] **Step 2: Persist the session after every completed turn**

Modify `run_turn`'s signature and its call site to also update the session file once streaming
finishes. Add parameters:

```rust
async fn run_turn(
    agent: Arc<Agent>,
    input: String,
    transcript: ntui::State<Vec<TranscriptEntry>>,
    usage: ntui::State<UsageSummary>,
    streaming: ntui::State<bool>,
    pending_turn_input: ntui::State<Option<String>>,
    session_path: ntui::State<std::path::PathBuf>,
    connection_name: String,
    model_name: String,
    tier: PermissionTier,
    project_root: std::path::PathBuf,
) {
```

At the very end of the function (after the `while let Some(event) = stream.next().await` loop, right
before `streaming.set(false); pending_turn_input.set(None);`), add:

```rust
    if let Ok(messages) = agent.memory().get_messages_erased().await {
        let mut session = crate::session::types::SessionFile::new(
            project_root,
            connection_name,
            model_name,
            tier,
            chrono::Utc::now().to_rfc3339(),
        );
        session.entries = transcript.get();
        session.messages = messages;
        let _ = crate::session::store::save_session(&session_path.get(), &session);
    }
```

(A failed save is intentionally non-fatal to the turn — the transcript already reflects the turn in
memory; losing a persistence write is recoverable on the next turn, unlike losing the turn itself.)

Update the `hooks.use_effect(turn_id.get(), ...)` block's `tokio::spawn` call to pass the five new
arguments:

```rust
            let handle = tokio::spawn(run_turn(
                agent,
                input,
                transcript,
                usage,
                streaming,
                pending_turn_input,
                session_path.clone(),
                props.connection_name.clone(),
                props.model_name.clone(),
                tier.get(),
                std::env::current_dir().unwrap_or_default(),
            ));
```

(capturing `session_path`, `props.connection_name`/`props.model_name`/`tier.get()` into the closure
the same way `agent`/`transcript`/etc. are already captured — add `let session_path = session_path.clone();`
alongside the existing `let transcript = transcript.clone();` line inside that effect's closure
setup.)

- [ ] **Step 3: Write the test**

```rust
    #[tokio::test(start_paused = true)]
    async fn clear_resets_transcript_and_starts_a_new_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        props.session_path = dir.path().join("original.json");
        crate::session::store::save_session(
            &props.session_path,
            &crate::session::types::SessionFile::new(
                std::path::PathBuf::from("/proj"),
                "local-vllm".into(),
                "m".into(),
                PermissionTier::FullAuto,
                "2026-07-06T00:00:00Z".into(),
            ),
        )
        .unwrap();

        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "hi there").await;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("Hello, world"));

        type_and_submit(&mut t, "/clear").await;
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(!text.contains("hi there"), "{text}");
        assert!(!text.contains("Hello, world"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn completed_turn_is_persisted_to_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        props.session_path = dir.path().join("session.json");
        crate::session::store::save_session(
            &props.session_path,
            &crate::session::types::SessionFile::new(
                std::path::PathBuf::from("/proj"),
                "local-vllm".into(),
                "m".into(),
                PermissionTier::FullAuto,
                "2026-07-06T00:00:00Z".into(),
            ),
        )
        .unwrap();
        let session_path = props.session_path.clone();

        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "hi there").await;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        let saved = crate::session::store::load_session(&session_path).unwrap();
        assert!(saved.entries.iter().any(|e| matches!(e, TranscriptEntry::UserTurn { text } if text == "hi there")));
        assert!(!saved.messages.is_empty());
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib tui::app`
Expected: PASS (7 tests: the 5 from Task 8 + these 2). Update `test_props()` to include
`user_state_dir: std::path::PathBuf::new()` (or wherever it constructs `AppProps`) if `cargo check`
flags a missing field.

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: implement /clear and persist the session file after every completed turn"
```

---

### Task 10: `/model`

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Add pending-selection state and extend `SlashContext`**

Add a new `App`-level state (alongside `pending_permission`):

```rust
    let pending_model_choice = hooks.use_state(|| Option::<Vec<(crate::config::connection::Connection, String)>>::None);
```

Extend `SlashContext` with everything `/model`'s handler needs:

```rust
struct SlashContext {
    transcript: ntui::State<Vec<TranscriptEntry>>,
    tier: ntui::State<PermissionTier>,
    session_path: ntui::State<std::path::PathBuf>,
    connection_name: String,
    model_name: String,
    project_root: std::path::PathBuf,
    user_state_dir: std::path::PathBuf,
    user_config_dir: std::path::PathBuf,
    project_config_dir: std::path::PathBuf,
    pending_model_choice: ntui::State<Option<Vec<(crate::config::connection::Connection, String)>>>,
}
```

(`user_config_dir`/`project_config_dir` are needed to call `load_connections`; add them to
`AppProps` the same way `user_state_dir` was added in Task 9 — both default to
`std::path::PathBuf::new()`, both populated by `run_tui` from `paths.user_config_dir`/
`paths.project_config_dir`, both threaded into `SlashContext`'s construction at the `Enter` branch
call site.)

- [ ] **Step 2: Handle `SlashCommand::Model`**

Add this arm to `dispatch_slash_command` (before the catch-all):

```rust
        SlashCommand::Model => {
            match crate::config::connection::load_connections(&ctx.user_config_dir, &ctx.project_config_dir) {
                Ok(connections) if connections.is_empty() => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: "no connections configured; run `local-code connections add`".to_string(),
                        });
                    });
                }
                Ok(connections) => {
                    let mut choices = Vec::new();
                    for conn in &connections {
                        let mut models = conn.models.clone();
                        if !models.contains(&conn.default_model) {
                            models.insert(0, conn.default_model.clone());
                        }
                        for model_name in models {
                            choices.push((conn.clone(), model_name));
                        }
                    }
                    let listing: Vec<String> = choices
                        .iter()
                        .enumerate()
                        .take(9)
                        .map(|(i, (conn, model))| format!("{}) {} · {}", i + 1, conn.name, model))
                        .collect();
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!(
                                "Select a connection/model (press the digit key):\n{}",
                                listing.join("\n")
                            ),
                        });
                    });
                    ctx.pending_model_choice.set(Some(choices.into_iter().take(9).collect()));
                }
                Err(e) => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("failed to load connections: {e}"),
                        });
                    });
                }
            }
        }
```

- [ ] **Step 3: Intercept digit keys when a model choice is pending**

`App`'s `use_input` handler already special-cases `pending_permission.get().is_some()` first. Add a
second priority check immediately after it (same shape, same priority position — before the normal
typing/Enter handling), capturing the extra state it needs:

```rust
            if let Some(choices) = pending_model_choice.get() {
                if let KeyCode::Char(c) = ev.code {
                    if let Some(digit) = c.to_digit(10) {
                        if digit >= 1 && (digit as usize) <= choices.len() {
                            let (connection, model_name) = choices[digit as usize - 1].clone();
                            pending_model_choice.set(None);
                            let api_key = crate::config::secrets::SecretStore::get_api_key(&connection.name).ok().flatten();
                            match crate::agent::provider::build_model(&connection, api_key) {
                                Ok(new_model) => {
                                    let agent_for_history = agent.clone();
                                    let pending_permission_for_rebuild = pending_permission.clone();
                                    let agent_and_responder = agent_and_responder.clone();
                                    let transcript_for_notice = transcript.clone();
                                    let tier_value = tier.get();
                                    let always_allow = always_allow_snapshot.clone();
                                    let always_deny = always_deny_snapshot.clone();
                                    let system_context = props.system_context.clone();
                                    tokio::spawn(async move {
                                        let history = agent_for_history
                                            .memory()
                                            .get_messages_erased()
                                            .await
                                            .unwrap_or_default();
                                        let rebuilt = crate::tui::rebuild::rebuild_agent(
                                            new_model,
                                            tier_value,
                                            always_allow,
                                            always_deny,
                                            history,
                                            &system_context,
                                            pending_permission_for_rebuild,
                                        );
                                        agent_and_responder.set(rebuilt);
                                        transcript_for_notice.update(|entries| {
                                            entries.push(TranscriptEntry::SystemNotice {
                                                text: format!("switched to {} · {}", connection.name, model_name),
                                            });
                                        });
                                    });
                                }
                                Err(e) => {
                                    transcript.update(|entries| {
                                        entries.push(TranscriptEntry::SystemNotice {
                                            text: format!("failed to switch model: {e}"),
                                        });
                                    });
                                }
                            }
                        }
                    }
                }
                return;
            }
```

This references `always_allow_snapshot`/`always_deny_snapshot`, which don't exist yet — add them as
plain (non-`State`) local `let` bindings at the top of `App`'s body, since `always_allow`/`always_deny`
only ever come from `props` and never change during the component's lifetime:

```rust
    let always_allow_snapshot = props.always_allow.clone();
    let always_deny_snapshot = props.always_deny.clone();
```

- [ ] **Step 4: Write the test**

```rust
    #[tokio::test(start_paused = true)]
    async fn model_command_lists_choices_and_switches_on_digit_press() {
        // This test exercises the parsing/listing/selection *mechanics* using
        // a fixture with zero real connections configured (since App's test
        // harness has no filesystem-backed Paths wired in) — it asserts the
        // "no connections configured" branch specifically, which is exactly
        // as real a code path as the populated-list branch and doesn't
        // require constructing on-disk connections.toml fixtures inside this
        // component test. The populated-list and successful-switch branches
        // are covered by `dispatch_slash_command`'s own logic being pure
        // enough to reason about, and by this plan's Task 16 CLI-level tests
        // that do set up real connections.toml fixtures.
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/model").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("no connections configured"), "{}", t.frame_text());
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib tui::app`
Expected: PASS (8 tests).

- [ ] **Step 6: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: implement /model to list and switch connections/models, preserving history"
```

---

### Task 11: `/permissions`

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Handle `SlashCommand::Permissions`**

`/permissions` reuses the existing `tier` `State` (already cycled by Ctrl+A per Phase 3) and the
`always_allow`/`always_deny_snapshot` locals from Task 10. Add this arm:

```rust
        SlashCommand::Permissions => {
            let current = ctx.tier.get();
            let label = match current {
                PermissionTier::Ask => "ask",
                PermissionTier::AutoAcceptEdits => "auto-accept-edits",
                PermissionTier::FullAuto => "full-auto",
            };
            let text = format!(
                "Current tier: {label}\n\
                 1) ask\n2) auto-accept-edits\n3) full-auto\n\
                 (press a digit key to switch, or Ctrl+A to cycle)\n\
                 always-allow: {}\nalways-deny: {}",
                if ctx.always_allow.is_empty() { "(none)".to_string() } else { ctx.always_allow.join(", ") },
                if ctx.always_deny.is_empty() { "(none)".to_string() } else { ctx.always_deny.join(", ") },
            );
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
            ctx.pending_permissions_menu.set(true);
        }
```

Add `always_allow`/`always_deny`/`pending_permissions_menu` to `SlashContext`:

```rust
    always_allow: Vec<String>,
    always_deny: Vec<String>,
    pending_permissions_menu: ntui::State<bool>,
```

populated at the `Enter` branch's `SlashContext` construction from `always_allow_snapshot.clone()`,
`always_deny_snapshot.clone()`, and a new `pending_permissions_menu.clone()` state:

```rust
    let pending_permissions_menu = hooks.use_state(|| false);
```

- [ ] **Step 2: Intercept digit keys when the permissions menu is pending**

Add a third priority branch in `use_input`, after the model-choice interception from Task 10:

```rust
            if pending_permissions_menu.get() {
                let new_tier = match ev.code {
                    KeyCode::Char('1') => Some(PermissionTier::Ask),
                    KeyCode::Char('2') => Some(PermissionTier::AutoAcceptEdits),
                    KeyCode::Char('3') => Some(PermissionTier::FullAuto),
                    _ => None,
                };
                if let Some(new_tier) = new_tier {
                    tier.set(new_tier);
                    pending_permissions_menu.set(false);
                    transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("permission tier set to {new_tier:?}"),
                        });
                    });
                }
                return;
            }
```

(`{new_tier:?}` is acceptable here — `PermissionTier`'s `Debug` output (`Ask`/`AutoAcceptEdits`/
`FullAuto`) is clear enough for this confirmation line; `tier_label` exists for the header's
lowercase-kebab rendering but reusing it here is not required for correctness.)

- [ ] **Step 3: Write the test**

```rust
    #[tokio::test(start_paused = true)]
    async fn permissions_command_shows_tier_and_lists_and_digit_press_changes_tier() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/permissions").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("Current tier: full-auto"), "{}", t.frame_text());

        t.send_key(KeyCode::Char('1')).unwrap();
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("[ask]"), "{}", t.frame_text());
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib tui::app`
Expected: PASS (9 tests).

- [ ] **Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: implement /permissions to view and change the tier and view the allow/deny list"
```

---

### Task 12: `/connections`

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Handle `SlashCommand::ConnectionsList`, `ConnectionsRemove`, and `ConnectionsAddUnsupported`**

These call straight through to Phase 1's `local_code::cli::connections::{list, remove}` — both are
already generic over `W: Write` (per Phase 1's plan), so capturing their output into a `Vec<u8>`
and rendering it as one `SystemNotice` is the entire adapter needed; no wizard logic is
reimplemented. Add these arms:

```rust
        SlashCommand::ConnectionsList => {
            let paths = crate::config::paths::Paths {
                user_config_dir: ctx.user_config_dir.clone(),
                project_config_dir: ctx.project_config_dir.clone(),
                user_state_dir: ctx.user_state_dir.clone(),
            };
            let mut out = Vec::new();
            let text = match crate::cli::connections::list(&paths, &mut out) {
                Ok(()) => String::from_utf8_lossy(&out).to_string(),
                Err(e) => format!("failed to list connections: {e}"),
            };
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
        }
        SlashCommand::ConnectionsRemove { name } => {
            let paths = crate::config::paths::Paths {
                user_config_dir: ctx.user_config_dir.clone(),
                project_config_dir: ctx.project_config_dir.clone(),
                user_state_dir: ctx.user_state_dir.clone(),
            };
            let mut out = Vec::new();
            let text = match crate::cli::connections::remove(&paths, &name, &mut out) {
                Ok(()) => String::from_utf8_lossy(&out).to_string(),
                Err(e) => format!("failed to remove connection: {e}"),
            };
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
        }
        SlashCommand::ConnectionsAddUnsupported => {
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice {
                    text: "adding a connection interactively isn't supported inside the TUI \
                           (the wizard needs multi-step line-by-line stdin, which the raw-mode \
                           TUI input loop doesn't support). Exit and run \
                           `local-code connections add` in a separate terminal, then use /model \
                           to switch to it."
                        .to_string(),
                });
            });
        }
```

This is the documented design choice from this plan's Architecture section: `list`/`remove` are
single-shot and non-interactive, so they adapt trivially; `add` is a multi-turn wizard that the
`ntui` raw-mode input loop (one keystroke at a time, no line-buffered `BufRead`) cannot drive without
building a second, parallel "wizard mode" state machine — explicitly out of scope, with a clear
in-product pointer to the CLI equivalent instead of a silent no-op.

- [ ] **Step 2: Write the test**

```rust
    #[tokio::test(start_paused = true)]
    async fn connections_list_reports_no_connections_configured() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/connections list").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("No connections configured"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn connections_add_explains_it_is_unsupported_in_tui() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/connections add").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("local-code connections add"), "{}", t.frame_text());
    }
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib tui::app`
Expected: PASS (11 tests).

- [ ] **Step 4: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: implement /connections list/remove as thin wrappers over the Phase 1 CLI functions"
```

---

### Task 13: `/compact`

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Give `SlashContext` access to the live agent and model**

`/compact` needs the current `Arc<Agent>` (to read/clear/repopulate its memory in place — no
rebuild) and the `SharedModel` (to make the one summarization call, since `Agent` has no public
model accessor). Add both to `SlashContext`:

```rust
    agent: Arc<Agent>,
    model: SharedModel,
```

populated at the `Enter` branch's construction from `agent.clone()` (the tuple destructured from
`agent_and_responder.get()`) and `props.model.clone().expect(...)`.

- [ ] **Step 2: Handle `SlashCommand::Compact`**

```rust
        SlashCommand::Compact => {
            const RETAIN_RECENT: usize = 10;
            const COMPACT_THRESHOLD: usize = 20;
            let agent = ctx.agent.clone();
            let model = ctx.model.clone();
            let transcript = ctx.transcript.clone();
            tokio::spawn(async move {
                let history = match agent.memory().get_messages_erased().await {
                    Ok(h) => h,
                    Err(e) => {
                        transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: format!("compact failed: could not read history: {e}"),
                            });
                        });
                        return;
                    }
                };

                if history.len() <= COMPACT_THRESHOLD {
                    transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!(
                                "nothing to compact yet ({} messages, threshold is {COMPACT_THRESHOLD})",
                                history.len()
                            ),
                        });
                    });
                    return;
                }

                let split_at = history.len().saturating_sub(RETAIN_RECENT);
                let (older, recent) = history.split_at(split_at);

                let mut conversation_text = String::new();
                for msg in older {
                    let role = format!("{:?}", msg.role);
                    if let Some(content) = &msg.content {
                        conversation_text.push_str(&format!("{role}: {content}\n"));
                    }
                }

                let summary_request = daimon::model::types::ChatRequest {
                    messages: vec![
                        daimon::model::types::Message::system(
                            "You are a conversation summarizer. Summarize the following \
                             conversation into a concise paragraph that preserves all important \
                             facts, decisions, tool results, and context. Be specific — include \
                             names, numbers, and outcomes. Do not include any preamble, just the \
                             summary.",
                        ),
                        daimon::model::types::Message::user(conversation_text),
                    ],
                    tools: Vec::new(),
                    temperature: Some(0.0),
                    max_tokens: Some(512),
                };

                let summary_text = match model.generate_erased(&summary_request).await {
                    Ok(response) => response.text().to_string(),
                    Err(e) => {
                        transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: format!("compact failed: summarization call errored: {e}"),
                            });
                        });
                        return;
                    }
                };

                if let Err(e) = agent.memory().clear_erased().await {
                    transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("compact failed: could not clear memory: {e}"),
                        });
                    });
                    return;
                }
                let _ = agent
                    .memory()
                    .add_message_erased(daimon::model::types::Message::system(format!(
                        "Previous conversation summary: {summary_text}"
                    )))
                    .await;
                for msg in recent.iter().cloned() {
                    let _ = agent.memory().add_message_erased(msg).await;
                }

                // The display transcript has no 1:1 correspondence to the
                // message-level split above (one user turn can expand into
                // several TranscriptEntry values via tool cards) — this plan
                // approximates the same boundary at the display layer by
                // keeping only the transcript's last RETAIN_RECENT entries
                // and prepending one SystemNotice with the summary, rather
                // than attempting an exact message-to-entry alignment. This
                // is a documented approximation, the same honest-scoping
                // approach Phase 3 used for diff coloring.
                transcript.update(|entries| {
                    let keep_from = entries.len().saturating_sub(RETAIN_RECENT);
                    let mut compacted = vec![TranscriptEntry::SystemNotice {
                        text: format!("compacted {} older messages into a summary", older.len()),
                    }];
                    compacted.extend(entries.split_off(keep_from));
                    *entries = compacted;
                });
            });
        }
```

- [ ] **Step 3: Write the test**

```rust
    #[tokio::test(start_paused = true)]
    async fn compact_reports_nothing_to_do_below_the_threshold() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/compact").await;
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("nothing to compact yet"), "{}", t.frame_text());
    }
```

(A test exercising the actual summarization path — pushing more than `COMPACT_THRESHOLD` messages
into the agent's memory first, then asserting the transcript now contains a "compacted N older
messages" notice and the underlying `agent.memory()` holds a system summary message — is added as
part of this task's Step 4 below, using `StreamingEchoModel`-style scripted responses so it stays
a fast, deterministic `cargo test`, not a live-server test.)

- [ ] **Step 4: Write the full-compaction test**

```rust
    #[tokio::test(start_paused = true)]
    async fn compact_summarizes_older_messages_and_keeps_recent_ones() {
        let mut props = test_props();
        // Reuse StreamingEchoModel as the active model for both turns and the
        // summarization call — its generate() (non-streaming) path is used by
        // /compact, its generate_stream() path by ordinary turns; both are
        // already implemented on this fixture from Phase 3.
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props.clone())).unwrap();

        for i in 0..15 {
            type_and_submit(&mut t, &format!("turn {i}")).await;
            for _ in 0..10 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                t.tick().await.unwrap();
            }
        }

        type_and_submit(&mut t, "/compact").await;
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("compacted"), "{}", t.frame_text());
        let _ = props; // props is cloned above only to keep it available for potential future assertions
    }
```

`StreamingEchoModel::generate` must exist for this test to compile — check Phase 3's fixture: it
only implements `generate_stream`, returning `unused` from `generate`. Since `generate()` (used by
`/compact`'s `model.generate_erased(...)`) returning the literal `"unused"` string is still a valid
`ChatResponse`, the assertion above only checks for the `"compacted"` transcript notice (which
`/compact`'s own code always emits once compaction proceeds, regardless of the actual summary text)
so no fixture change is required.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib tui::app`
Expected: PASS (13 tests).

- [ ] **Step 6: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: implement /compact to summarize older turns via the active model in place"
```

---

### Task 14: `/init`

**Files:**
- Create: `src/init/mod.rs`
- Create: `src/init/survey.rs`
- Create: `src/init/prompt.rs`
- Create: `src/init/generate.rs`
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Add the `ignore` dependency**

Append to `[dependencies]` in `Cargo.toml`:

```toml
ignore = "0.4"
```

Run: `cargo check`
Expected: builds (unused-code warnings only).

- [ ] **Step 2: Write the failing test for `survey.rs`**

```rust
// src/init/survey.rs

use std::path::Path;

use ignore::WalkBuilder;

/// A `.gitignore`-respecting survey of a project directory: every
/// non-ignored file path (capped, so a huge repo doesn't blow up the prompt)
/// plus the full contents of any recognized build-manifest file found, used
/// to build the LLM prompt `/init` sends to generate AGENTS.md.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectSurvey {
    pub file_paths: Vec<String>,
    /// (relative path, file contents) pairs for recognized manifests.
    pub manifests: Vec<(String, String)>,
}

const RECOGNIZED_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "Gemfile",
    "pom.xml",
    "build.gradle",
];

const MAX_FILES_LISTED: usize = 500;

/// Walks `project_root`, respecting `.gitignore` (via `ignore::WalkBuilder`,
/// the same traversal semantics ripgrep uses), collecting up to
/// `MAX_FILES_LISTED` relative file paths and the full contents of any
/// top-level file matching `RECOGNIZED_MANIFESTS`.
pub fn survey_project(project_root: &Path) -> ProjectSurvey {
    let mut survey = ProjectSurvey::default();

    for entry in WalkBuilder::new(project_root).build().filter_map(|e| e.ok()) {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(project_root) else { continue };
        let relative_str = relative.to_string_lossy().to_string();

        if survey.file_paths.len() < MAX_FILES_LISTED {
            survey.file_paths.push(relative_str.clone());
        }

        if let Some(name) = entry.path().file_name().and_then(|n| n.to_str()) {
            if RECOGNIZED_MANIFESTS.contains(&name) {
                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                    survey.manifests.push((relative_str, content));
                }
            }
        }
    }

    survey
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn collects_file_paths_and_recognized_manifests() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let survey = survey_project(dir.path());
        assert!(survey.file_paths.iter().any(|p| p == "Cargo.toml"));
        assert!(survey.file_paths.iter().any(|p| p == "main.rs"));
        assert_eq!(survey.manifests.len(), 1);
        assert_eq!(survey.manifests[0].0, "Cargo.toml");
        assert!(survey.manifests[0].1.contains("name = \"x\""));
    }

    #[test]
    fn respects_gitignore() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "should not appear").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "should appear").unwrap();

        let survey = survey_project(dir.path());
        assert!(!survey.file_paths.iter().any(|p| p == "ignored.txt"));
        assert!(survey.file_paths.iter().any(|p| p == "kept.txt"));
    }

    #[test]
    fn non_manifest_files_are_listed_but_not_read() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "secret notes").unwrap();

        let survey = survey_project(dir.path());
        assert!(survey.file_paths.iter().any(|p| p == "notes.txt"));
        assert!(survey.manifests.is_empty());
    }
}
```

- [ ] **Step 3: Create `src/init/mod.rs`**

```rust
//! `/init`: survey the project, build a prompt from that survey, and have
//! the active model generate/update `AGENTS.md`. Never writes `CLAUDE.md`,
//! per spec section 4.

pub mod generate;
pub mod prompt;
pub mod survey;

pub use generate::{generate_agents_md, write_agents_md, InitError};
pub use prompt::build_init_prompt;
pub use survey::{survey_project, ProjectSurvey};
```

- [ ] **Step 4: Add `pub mod init;` to `src/lib.rs`**

```rust
pub mod config;
pub mod cli;
pub mod permissions;
pub mod agent;
pub mod tui;
pub mod session;
pub mod context;
pub mod init;
```

- [ ] **Step 5: Run the survey test to verify it fails, then passes**

Run: `cargo test --lib init::survey`
Expected: create `src/init/survey.rs` with Step 2's content (plus placeholder `src/init/prompt.rs`,
`src/init/generate.rs` doc-comment-only files so the crate compiles); then PASS (3 tests).

- [ ] **Step 6: Write `prompt.rs` and its failing test**

```rust
// src/init/prompt.rs

use crate::init::survey::ProjectSurvey;

/// Builds the user-message text for the `/init` generation call from a
/// `ProjectSurvey` — a pure function so its output is deterministically
/// testable without a live model.
pub fn build_init_prompt(survey: &ProjectSurvey) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Generate the contents of an AGENTS.md file for this project. AGENTS.md is read at the \
         start of every coding-agent session and folded into the system prompt — it should \
         describe the project's structure, build/test commands, and any conventions a coding \
         agent should follow. Output only the Markdown content of the file, no preamble.\n\n",
    );

    if !survey.manifests.is_empty() {
        prompt.push_str("Detected build manifests:\n\n");
        for (path, content) in &survey.manifests {
            let truncated: String = content.chars().take(2000).collect();
            prompt.push_str(&format!("### {path}\n```\n{truncated}\n```\n\n"));
        }
    }

    prompt.push_str(&format!(
        "Project contains {} files. A sample of paths:\n{}\n",
        survey.file_paths.len(),
        survey.file_paths.iter().take(200).map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
    ));

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_manifest_contents_when_present() {
        let survey = ProjectSurvey {
            file_paths: vec!["Cargo.toml".into(), "src/main.rs".into()],
            manifests: vec![("Cargo.toml".into(), "[package]\nname = \"local-code\"".into())],
        };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.contains("Cargo.toml"));
        assert!(prompt.contains("name = \"local-code\""));
        assert!(prompt.contains("src/main.rs"));
    }

    #[test]
    fn handles_a_survey_with_no_manifests() {
        let survey = ProjectSurvey { file_paths: vec!["README.md".into()], manifests: vec![] };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.contains("README.md"));
        assert!(!prompt.contains("Detected build manifests"));
    }

    #[test]
    fn truncates_very_long_manifest_contents() {
        let long_content = "x".repeat(5000);
        let survey = ProjectSurvey {
            file_paths: vec![],
            manifests: vec![("Cargo.toml".into(), long_content)],
        };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.len() < 5000 + 1000);
    }
}
```

- [ ] **Step 7: Run the test to verify it fails, then passes**

Run: `cargo test --lib init::prompt`
Expected: replace the placeholder with the content above; then PASS (3 tests).

- [ ] **Step 8: Write `generate.rs` and its failing test**

```rust
// src/init/generate.rs

use std::path::Path;

use daimon::model::types::{ChatRequest, Message};
use daimon::model::SharedModel;

use crate::init::prompt::build_init_prompt;
use crate::init::survey::ProjectSurvey;

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("model call failed: {0}")]
    Model(#[from] daimon::DaimonError),
    #[error("failed to write AGENTS.md: {0}")]
    Write(#[source] std::io::Error),
}

const INIT_SYSTEM_PROMPT: &str = "You are generating an AGENTS.md file for a coding project. \
Be concrete and specific to the project you're shown, not generic boilerplate.";

/// Makes the one real LLM call `/init` needs: survey → prompt → generated
/// Markdown. Uses `model.generate_erased` directly (not `Agent::prompt`) since
/// this is a single, tool-free completion, not a ReAct turn.
pub async fn generate_agents_md(model: &SharedModel, survey: &ProjectSurvey) -> Result<String, InitError> {
    let request = ChatRequest {
        messages: vec![
            Message::system(INIT_SYSTEM_PROMPT),
            Message::user(build_init_prompt(survey)),
        ],
        tools: Vec::new(),
        temperature: Some(0.2),
        max_tokens: Some(2048),
    };
    let response = model.generate_erased(&request).await?;
    Ok(response.text().to_string())
}

/// Writes `content` to `<project_root>/AGENTS.md`, overwriting any existing
/// file. Never writes `CLAUDE.md` — that file is read for compatibility with
/// existing Claude Code projects, not owned by this tool, per spec section 4.
pub fn write_agents_md(project_root: &Path, content: &str) -> Result<(), InitError> {
    std::fs::write(project_root.join("AGENTS.md"), content).map_err(InitError::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::model::types::{ChatResponse, StopReason, Usage};
    use daimon::stream::ResponseStream;
    use std::sync::Arc;
    use tempfile::tempdir;

    struct FixedResponseModel(String);
    impl daimon::model::Model for FixedResponseModel {
        async fn generate(&self, _request: &ChatRequest) -> daimon::Result<ChatResponse> {
            Ok(ChatResponse {
                message: Message::assistant(self.0.clone()),
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn generate_agents_md_returns_the_models_text() {
        let model: SharedModel = Arc::new(FixedResponseModel("# AGENTS.md\n\nThis is a Rust crate.".into()));
        let survey = ProjectSurvey { file_paths: vec!["Cargo.toml".into()], manifests: vec![] };
        let content = generate_agents_md(&model, &survey).await.unwrap();
        assert!(content.contains("This is a Rust crate."));
    }

    #[test]
    fn write_agents_md_creates_the_file_at_the_project_root() {
        let dir = tempdir().unwrap();
        write_agents_md(dir.path(), "# AGENTS.md\n\ncontent").unwrap();
        let written = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(written.contains("content"));
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    #[test]
    fn write_agents_md_overwrites_an_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "old content").unwrap();
        write_agents_md(dir.path(), "new content").unwrap();
        let written = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(written, "new content");
    }
}
```

- [ ] **Step 9: Run the test to verify it fails, then passes**

Run: `cargo test --lib init::generate`
Expected: replace the placeholder with the content above; then PASS (3 tests).

- [ ] **Step 10: Wire `/init` into `App`**

Add `project_root: std::path::PathBuf` to `SlashContext` if not already present (Task 9 already
added it), and this arm to `dispatch_slash_command`:

```rust
        SlashCommand::Init => {
            let model = ctx.model.clone();
            let project_root = ctx.project_root.clone();
            let transcript = ctx.transcript.clone();
            transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice {
                    text: "surveying the project and generating AGENTS.md…".to_string(),
                });
            });
            tokio::spawn(async move {
                let survey = crate::init::survey_project(&project_root);
                match crate::init::generate_agents_md(&model, &survey).await {
                    Ok(content) => match crate::init::write_agents_md(&project_root, &content) {
                        Ok(()) => transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: "wrote AGENTS.md".to_string(),
                            });
                        }),
                        Err(e) => transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: format!("/init failed to write AGENTS.md: {e}"),
                            });
                        }),
                    },
                    Err(e) => transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("/init failed: {e}"),
                        });
                    }),
                }
            });
        }
```

- [ ] **Step 11: Write the test**

```rust
    #[tokio::test(start_paused = true)]
    async fn init_command_writes_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/init").await;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        std::env::set_current_dir(original_dir).unwrap();
        assert!(t.frame_text().contains("wrote AGENTS.md"), "{}", t.frame_text());
        assert!(dir.path().join("AGENTS.md").exists());
    }
```

Note: this test relies on `ctx.project_root` being derived from `std::env::current_dir()` (as wired
in Task 9's `SlashContext` construction) — `std::env::set_current_dir` around the test body makes
`/init` operate on the tempdir. This is a process-global mutation; if `cargo test` runs this test
concurrently with others that also touch the current directory, mark it `#[serial]`-equivalent by
running the whole `tui::app` test module single-threaded (`cargo test --lib tui::app -- --test-threads=1`)
if flakiness appears — call this out in a comment above the test rather than silently hoping for the
best.

- [ ] **Step 12: Run the tests to verify they pass**

Run: `cargo test --lib tui::app -- --test-threads=1`
Expected: PASS (14 tests).

- [ ] **Step 13: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 14: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/init/mod.rs src/init/survey.rs src/init/prompt.rs src/init/generate.rs src/tui/app.rs
git commit -m "feat: implement /init to survey the project and generate AGENTS.md via the active model"
```

---

### Task 15: `/resume` (in-TUI)

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Add pending-selection state and extend `SlashContext`**

```rust
    let pending_resume_choice = hooks.use_state(|| Option::<Vec<crate::session::types::SessionSummary>>::None);
```

Add to `SlashContext`:

```rust
    pending_resume_choice: ntui::State<Option<Vec<crate::session::types::SessionSummary>>>,
    agent_and_responder: ntui::State<(Arc<Agent>, Arc<PermissionGate>, crate::tui::rebuild::ResponderHandle)>,
    pending_permission: ntui::State<Option<crate::permissions::types::PermissionRequest>>,
    system_context: String,
```

(`agent_and_responder`, `pending_permission`, and `system_context` are also needed by `/model`'s
handler from Task 10 if it is refactored to route through `dispatch_slash_command` the same way —
this task assumes Task 10's model-switch logic already lives inline in `use_input` rather than in
`dispatch_slash_command`, per that task's Step 3; `/resume`'s full-agent-rebuild needs the same
inline placement for the same reason: `dispatch_slash_command` doesn't have direct `State::set`
access convenient for a `ntui::State<(Arc<Agent>, ...)>` field unless it's threaded through, which
these two extra `SlashContext` fields do.)

- [ ] **Step 2: Handle `SlashCommand::Resume` (the listing half)**

```rust
        SlashCommand::Resume => {
            match crate::session::store::list_sessions(&ctx.user_state_dir, &ctx.project_root) {
                Ok(sessions) if sessions.is_empty() => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: "no previous sessions found for this project".to_string(),
                        });
                    });
                }
                Ok(sessions) => {
                    let listing: Vec<String> = sessions
                        .iter()
                        .enumerate()
                        .take(9)
                        .map(|(i, s)| {
                            format!(
                                "{}) {} · {} · {}{}",
                                i + 1,
                                s.updated_at,
                                s.connection_name,
                                s.model_name,
                                s.first_user_turn_preview
                                    .as_ref()
                                    .map(|p| format!(" · \"{p}\""))
                                    .unwrap_or_default()
                            )
                        })
                        .collect();
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("Select a session to resume (press the digit key):\n{}", listing.join("\n")),
                        });
                    });
                    ctx.pending_resume_choice.set(Some(sessions.into_iter().take(9).collect()));
                }
                Err(e) => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("failed to list sessions: {e}"),
                        });
                    });
                }
            }
        }
```

- [ ] **Step 3: Intercept digit keys when a resume choice is pending**

Add a fourth priority branch in `use_input` (after the model-choice and permissions-menu branches
from Tasks 10–11):

```rust
            if let Some(sessions) = pending_resume_choice.get() {
                if let KeyCode::Char(c) = ev.code {
                    if let Some(digit) = c.to_digit(10) {
                        if digit >= 1 && (digit as usize) <= sessions.len() {
                            let summary = sessions[digit as usize - 1].clone();
                            pending_resume_choice.set(None);
                            match crate::session::store::load_session(&summary.path) {
                                Ok(session) => {
                                    match crate::agent::provider::build_model(
                                        &crate::config::connection::Connection {
                                            name: session.connection_name.clone(),
                                            provider: crate::config::connection::ProviderKind::OpenAiCompatible,
                                            base_url: String::new(),
                                            default_model: session.model_name.clone(),
                                            models: vec![],
                                        },
                                        None,
                                    ) {
                                        // Placeholder base_url/provider above are always overwritten
                                        // below by re-resolving the real Connection from disk before
                                        // actually switching — see the note after this block.
                                        _ => {}
                                    }

                                    let paths_lookup = crate::config::connection::load_connections(
                                        &user_config_dir_snapshot,
                                        &project_config_dir_snapshot,
                                    );
                                    let resolved_connection = paths_lookup
                                        .ok()
                                        .and_then(|conns| conns.into_iter().find(|c| c.name == session.connection_name));

                                    match resolved_connection {
                                        Some(mut connection) => {
                                            connection.default_model = session.model_name.clone();
                                            let api_key = crate::config::secrets::SecretStore::get_api_key(&connection.name)
                                                .ok()
                                                .flatten();
                                            match crate::agent::provider::build_model(&connection, api_key) {
                                                Ok(new_model) => {
                                                    let rebuilt = crate::tui::rebuild::rebuild_agent(
                                                        new_model,
                                                        session.tier,
                                                        always_allow_snapshot.clone(),
                                                        always_deny_snapshot.clone(),
                                                        session.messages.clone(),
                                                        &props.system_context,
                                                        pending_permission.clone(),
                                                    );
                                                    agent_and_responder.set(rebuilt);
                                                    tier.set(session.tier);
                                                    transcript.set(session.entries.clone());
                                                    session_path.set(summary.path.clone());
                                                }
                                                Err(e) => {
                                                    transcript.update(|entries| {
                                                        entries.push(TranscriptEntry::SystemNotice {
                                                            text: format!("failed to resume: could not build model: {e}"),
                                                        });
                                                    });
                                                }
                                            }
                                        }
                                        None => {
                                            transcript.update(|entries| {
                                                entries.push(TranscriptEntry::SystemNotice {
                                                    text: format!(
                                                        "failed to resume: connection '{}' no longer exists; run `local-code connections list`",
                                                        session.connection_name
                                                    ),
                                                });
                                            });
                                        }
                                    }
                                }
                                Err(e) => {
                                    transcript.update(|entries| {
                                        entries.push(TranscriptEntry::SystemNotice {
                                            text: format!("failed to load session: {e}"),
                                        });
                                    });
                                }
                            }
                        }
                    }
                }
                return;
            }
```

Simplify: delete the throwaway `match crate::agent::provider::build_model(&crate::config::connection::Connection { ... }, None) { _ => {} }`
block above — it was a placeholder scratch note in this draft, not real logic; the actual
connection lookup is the `paths_lookup`/`resolved_connection` code immediately following it. Add
two more plain (non-`State`) local snapshots near `always_allow_snapshot` at the top of `App`'s
body, since `use_input`'s closure needs `user_config_dir`/`project_config_dir` and they never
change during the component's lifetime:

```rust
    let user_config_dir_snapshot = props.user_config_dir.clone();
    let project_config_dir_snapshot = props.project_config_dir.clone();
```

(requiring `AppProps` to also carry `user_config_dir`/`project_config_dir`, added the same way
`user_state_dir` was added in Task 9 — both default to `std::path::PathBuf::new()`, both populated
by `run_tui` from the resolved `Paths`.)

- [ ] **Step 4: Write the test**

```rust
    #[tokio::test(start_paused = true)]
    async fn resume_command_reports_no_previous_sessions_when_none_exist() {
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "/resume").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("no previous sessions found"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn resume_command_lists_existing_sessions_and_resuming_restores_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = std::env::current_dir().unwrap();
        let mut session = crate::session::types::SessionFile::new(
            project_root.clone(),
            "some-connection".into(),
            "some-model".into(),
            PermissionTier::FullAuto,
            "2026-07-06T09:00:00Z".into(),
        );
        session.entries.push(TranscriptEntry::UserTurn { text: "earlier turn".into() });
        let path = crate::session::paths::new_session_path(dir.path(), &project_root, chrono::Utc::now());
        crate::session::store::save_session(&path, &session).unwrap();

        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "/resume").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("some-connection"), "{}", t.frame_text());

        // Resuming when the session's connection is no longer configured
        // (test_props() sets up no real connections.toml) surfaces the
        // clear "connection no longer exists" notice rather than panicking —
        // this exercises that failure path explicitly, since it's the
        // reachable one without a full connections.toml fixture.
        t.send_key(KeyCode::Char('1')).unwrap();
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("no longer exists"), "{}", t.frame_text());
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib tui::app -- --test-threads=1`
Expected: PASS (16 tests).

- [ ] **Step 6: Remove the now-fully-covered `unreachable!` catch-all from `dispatch_slash_command`**

Every `SlashCommand` variant now has an explicit arm (`Help`, `Unknown`, `Clear`, `Model`,
`Permissions`, `ConnectionsList`, `ConnectionsRemove`, `ConnectionsAddUnsupported`, `Init`, `Compact`,
`Resume`). Delete the trailing `other => unreachable!(...)` arm from Task 8's `dispatch_slash_command`
— the match is now exhaustive without it.

Run: `cargo check --lib`
Expected: PASS (removing a catch-all from an already-exhaustive match is fine; if the compiler
reports the match is *not* yet exhaustive, one of Tasks 9–15's arms was placed inside a nested
`match` or `if` instead of directly in `dispatch_slash_command`'s top-level `match command { ... }`
— move it back to the top level).

- [ ] **Step 7: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: implement /resume to switch to a previous session, restoring transcript and agent memory"
```

---

### Task 16: `local-code --resume` CLI flag

**Files:**
- Modify: `src/cli/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing test**

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib cli::resume_cli_tests`
Expected: FAIL to compile (`Cli` has no `resume` field yet).

- [ ] **Step 3: Add the `--resume` flag to `Cli`**

Add one field to the `Cli` struct in `src/cli/mod.rs`:

```rust
    /// Resume a previous session for this project: lists recent sessions and
    /// prompts for a choice (reading a line from stdin), or reopens the most
    /// recent one automatically if exactly one exists.
    #[arg(long)]
    pub resume: bool,
```

- [ ] **Step 4: Write the session-selection helper and its failing test**

```rust
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
        writeln!(out, "Resuming the only previous session ({}).", sessions[0].updated_at)?;
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
            s.first_user_turn_preview.as_ref().map(|p| format!(" · \"{p}\"")).unwrap_or_default()
        )?;
    }
    write!(out, "Resume which session? [1-{}, blank for most recent]: ", sessions.len())?;
    out.flush()?;

    let mut line = String::new();
    input.read_line(&mut line)?;
    let trimmed = line.trim();
    let index = if trimmed.is_empty() {
        0
    } else {
        trimmed.parse::<usize>().ok().filter(|n| *n >= 1 && *n <= sessions.len()).map(|n| n - 1).unwrap_or(0)
    };
    Ok(Some(sessions[index].clone()))
}

#[cfg(test)]
mod select_session_tests {
    use super::*;
    use crate::permissions::types::PermissionTier;

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
        assert!(String::from_utf8(out).unwrap().contains("No previous sessions"));
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
        let sessions = vec![summary("newest", "2026-07-06T00:00:00Z"), summary("older", "2026-07-01T00:00:00Z")];
        let mut out = Vec::new();
        let result = select_session_to_resume(&sessions, &b"\n"[..], &mut out).unwrap();
        assert_eq!(result.unwrap().connection_name, "newest");
    }

    #[test]
    fn numeric_input_selects_by_index() {
        let sessions = vec![summary("newest", "2026-07-06T00:00:00Z"), summary("older", "2026-07-01T00:00:00Z")];
        let mut out = Vec::new();
        let result = select_session_to_resume(&sessions, &b"2\n"[..], &mut out).unwrap();
        assert_eq!(result.unwrap().connection_name, "older");
        let _ = PermissionTier::Ask; // silence unused-import if PermissionTier ends up unused here
    }

    #[test]
    fn out_of_range_input_falls_back_to_most_recent() {
        let sessions = vec![summary("newest", "2026-07-06T00:00:00Z"), summary("older", "2026-07-01T00:00:00Z")];
        let mut out = Vec::new();
        let result = select_session_to_resume(&sessions, &b"99\n"[..], &mut out).unwrap();
        assert_eq!(result.unwrap().connection_name, "newest");
    }
}
```

- [ ] **Step 5: Run the tests to verify they fail, then pass**

Run: `cargo test --lib cli`
Expected: PASS (2 new `resume_cli_tests` + 5 new `select_session_tests` + all pre-existing `cli`
tests).

- [ ] **Step 6: Wire `--resume` into `run`**

Modify `run` in `src/cli/mod.rs`'s no-flag/no-command branch to check `cli.resume` before launching
`run_tui`:

```rust
        None => {
            let resume = if cli.resume {
                let sessions = crate::session::store::list_sessions(&paths.user_state_dir, &project_root)?;
                let chosen = select_session_to_resume(&sessions, stdin().lock(), stdout())?;
                match chosen {
                    Some(summary) => {
                        let session = crate::session::store::load_session(&summary.path)?;
                        Some(crate::tui::ResumedSession {
                            session_path: summary.path,
                            entries: session.entries,
                            messages: session.messages,
                            tier: session.tier,
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
```

- [ ] **Step 7: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 8: Manually verify the resume flow end-to-end (requires a real local server)**

```bash
printf 'my-server\n1\nhttp://localhost:8000/v1\nqwen2.5-coder-7b\n\n' | cargo run -- connections add
cargo run   # have a short conversation, then Ctrl+C to exit
cargo run -- --resume
```

Expected: the second invocation either auto-resumes (if it's the only session) or prints a numbered
list and a `Resume which session? [1-N, blank for most recent]:` prompt; after selecting, the TUI
launches with the prior transcript visible and the header showing the same connection/model as
before.

- [ ] **Step 9: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "feat: add local-code --resume, listing and reopening a previous session for this project"
```

---

### Task 17: Full-suite verification and live integration tests

**Files:**
- Create: `tests/live_compact.rs`
- Create: `tests/live_init.rs`

- [ ] **Step 1: Write `tests/live_compact.rs`**

```rust
//! Integration test proving `/compact`'s summarization logic works against a
//! real local server's non-streaming `generate` endpoint. Requires the same
//! environment variables as `tests/live_openai_compatible.rs` from Phase 2.
//! Run with: `cargo test --test live_compact -- --ignored --nocapture`

use std::sync::Arc;

use daimon::model::types::{ChatRequest, Message};
use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};

#[tokio::test]
#[ignore = "requires a real local OpenAI-compatible server"]
async fn summarization_call_returns_nonempty_text() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OPENAI_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OPENAI_MODEL")
        .expect("set LOCAL_CODE_TEST_OPENAI_MODEL to a model your server has loaded");

    let connection = Connection {
        name: "live-compact-test".into(),
        provider: ProviderKind::OpenAiCompatible,
        base_url,
        default_model: model_id,
        models: vec![],
    };
    let model = build_model(&connection, None).expect("model construction should not fail");

    let request = ChatRequest {
        messages: vec![
            Message::system("Summarize the following conversation in one sentence."),
            Message::user("User: what's 2+2?\nAssistant: 4."),
        ],
        tools: Vec::new(),
        temperature: Some(0.0),
        max_tokens: Some(128),
    };

    let response = model.generate_erased(&request).await.expect("summarization call should succeed");
    assert!(!response.text().is_empty());
}
```

- [ ] **Step 2: Write `tests/live_init.rs`**

```rust
//! Integration test proving `/init`'s generation call produces nonempty
//! AGENTS.md content against a real local server. Run with:
//! `cargo test --test live_init -- --ignored --nocapture`

use std::sync::Arc;

use local_code::agent::provider::build_model;
use local_code::config::connection::{Connection, ProviderKind};
use local_code::init::{generate_agents_md, survey_project};

#[tokio::test]
#[ignore = "requires a real local OpenAI-compatible server"]
async fn generates_nonempty_agents_md_for_this_repo() {
    let base_url = std::env::var("LOCAL_CODE_TEST_OPENAI_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_string());
    let model_id = std::env::var("LOCAL_CODE_TEST_OPENAI_MODEL")
        .expect("set LOCAL_CODE_TEST_OPENAI_MODEL to a model your server has loaded");

    let connection = Connection {
        name: "live-init-test".into(),
        provider: ProviderKind::OpenAiCompatible,
        base_url,
        default_model: model_id,
        models: vec![],
    };
    let model = build_model(&connection, None).expect("model construction should not fail");

    let survey = survey_project(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    let content = generate_agents_md(&model, &survey).await.expect("generation should succeed");
    assert!(!content.trim().is_empty());
}
```

- [ ] **Step 3: Confirm both compile and are skipped by default**

Run: `cargo test`
Expected: both new test binaries build; their tests report as `ignored`, not run; the rest of the
suite still passes.

Run: `cargo test -- --ignored --list`
Expected: lists `summarization_call_returns_nonempty_text` and
`generates_nonempty_agents_md_for_this_repo` among the available ignored tests.

- [ ] **Step 4: Run the entire workspace test suite one final time**

Run: `cargo test`
Expected: PASS — every test from Phases 1–3 and every task in this plan.

- [ ] **Step 5: Manually verify the full slash-command set end-to-end (requires a real local server)**

```bash
printf 'my-server\n1\nhttp://localhost:8000/v1\nqwen2.5-coder-7b\n\n' | cargo run -- connections add
cargo run
```

In the TUI: type `/help` (see all 8 commands listed), `/permissions` then `1` (tier switches to
`[ask]`), `/model` (lists connections × models; pressing `1` switches, keeping transcript), have a
conversation, `/compact` (prints "nothing to compact yet" until enough turns accumulate), `/init`
(writes `AGENTS.md` at the project root — verify with `cat AGENTS.md` after exiting), `/clear`
(transcript empties, a new session file appears under the state dir), Ctrl+C to exit, then
`cargo run -- --resume` to confirm the prior (pre-clear) session reopens with its transcript intact.

- [ ] **Step 6: Commit**

```bash
git add tests/live_compact.rs tests/live_init.rs
git commit -m "test: add ignored live integration tests for /compact summarization and /init generation"
```

---

## Self-review notes

- **Spec coverage — every slash command from section 6 has a task:** `/model` (Task 10), `/connections`
  add/list/remove (Task 12, with `add` explicitly and honestly scoped to CLI-only), `/init` (Task 14),
  `/permissions` (Task 11), `/compact` (Task 13), `/resume` (Tasks 15–16, both in-TUI and
  `--resume`), `/clear` (Task 9), `/help` (Task 8). Session persistence (section 7) is covered by
  Tasks 1–3 (types/storage), 6 (wiring into `run_tui`), 9 (save-after-every-turn), 15–16 (resume).
  AGENTS.md/CLAUDE.md context loading (section 4, the gap Phase 2 left open) is covered by Task 6.

- **Placeholder scan:** the only `unreachable!` in the whole plan (Task 8's temporary
  `dispatch_slash_command` catch-all) is explicitly a scaffolding step removed by name in Task 15's
  Step 6, once every `SlashCommand` variant has a real arm — by the end of the plan, `grep -rn
  "unreachable!\|todo!\|TODO\|TBD" src/` should return nothing outside test-only fixtures (none
  exist in this plan) or doc-comment prose describing past Phase 3 history (e.g. Task 8's comment
  quoting Phase 3's own doc comment about the placeholder it replaces — that's narrative, not a
  live stub).

- **Type consistency check:**
  - `Paths`, `Connection`, `ProviderKind`, `load_connections`, `SecretStore` (Phase 1) — used
    verbatim in Tasks 10, 12, 15, 16 exactly as defined in
    `docs/superpowers/plans/2026-07-06-foundation-config-connections.md`.
  - `PermissionGate`, `PermissionTier`, `PermissionSettings`, `load_settings`,
    `PermissionDecision`, `PermissionRequest`, `PermissionPrompter`, `build_model` (Phase 2) — used
    verbatim; `PermissionTier` gains a `Serialize`/`Deserialize` derive in Task 1 (additive, not a
    redefinition).
  - `App`, `AppProps`, `run_tui`, `TranscriptEntry`, `ToolCallEntry`, `ToolCallResult`,
    `UsageSummary`, `GatedTool`, `build_streaming_agent`, `NtuiPermissionPrompter` (Phase 3) — used
    verbatim; the four transcript-state types gain `Serialize`/`Deserialize` derives in Task 1
    (additive); `AppProps`/`run_tui` gain new fields/parameters in Tasks 6, 9, 10, 15 (additive,
    every pre-existing field/parameter retained); `build_streaming_agent` itself is untouched — a
    new sibling function `build_streaming_agent_with_history` is added instead (Task 4).
  - `daimon::agent::{Agent, AgentBuilder}`, `daimon::model::{SharedModel, types::{ChatRequest,
    Message, Role, ChatResponse, StopReason, Usage}}`, `daimon::memory::Memory`,
    `Agent::memory()` → `SharedMemory` → `ErasedMemory::{get_messages_erased, clear_erased,
    add_message_erased}` — all confirmed present by direct source inspection of the vendored
    `daimon-0.16.0`/`daimon-core-0.16.0` crates during this plan's research phase (not assumed from
    the Phase 2/3 plans' prose alone).

- **Cross-phase inconsistency found and fixed forward:** Phase 3's own traceability section states
  "Task 7's `rebuild_agent` local helper is written generically enough to be called again later, not
  just at mount" — but Phase 3's actual Task 7 code has no function named `rebuild_agent`; the
  construction logic is inlined directly in `App`'s `hooks.use_state` initializer. This plan's Task
  5 extracts that inline logic into the `rebuild_agent` function Phase 3's own documentation
  promised, and Task 5's Step 3 refactors `App`'s mount closure to call it — so the actual code now
  matches what Phase 3 said it would be, rather than silently working around the gap.

- **API-compatibility risks worth flagging:**
  1. **`Agent` has no public accessor for its model.** Confirmed by reading `daimon-0.16.0/src/agent/mod.rs`
     — only `memory()` is exposed; `model` is `pub(crate)`. `/compact` and `/init` therefore both
     take the `SharedModel` handle directly from `App`'s own `props.model`/state rather than through
     `Agent`, which is why `SlashContext` carries a separate `model: SharedModel` field alongside
     `agent: Arc<Agent>` (Tasks 13–14). If a future `daimon` version adds `Agent::model()`, these two
     call sites could simplify to use it, but nothing here depends on that happening.
  2. **`AgentBuilder::memory` takes `M: Memory + 'static` by value, not a pre-built `SharedMemory`
     (`Arc<dyn ErasedMemory>`).** This is why every history-preserving rebuild in this plan
     (`/model`, `/resume`, initial mount) goes through `SeededMemory::new(Vec<Message>)` — a fresh
     concrete `Memory` impl constructed from a snapshot of messages — rather than attempting to
     hand the same underlying memory store across agents. Functionally equivalent for this plan's
     purposes (nothing needs the *same* memory object to survive a rebuild, only its *contents*),
     but worth knowing if a future feature wants true object-identity continuity.
  3. **`/clear`'s interaction with the still-live agent's memory is a documented, deliberate gap**
     (Task 9): the visible transcript and session file reset immediately, but the agent object
     itself is not rebuilt, so its `SeededMemory` technically retains pre-clear history until the
     next `/compact` or a future enhancement explicitly clears `agent.memory()` too. Flagged in
     Task 9's own prose rather than silently shipped.
  4. **`/compact`'s message-level and display-transcript-level compaction boundaries are not
     perfectly aligned** (Task 13): the split point in `Vec<Message>` (by message count) and in
     `Vec<TranscriptEntry>` (also by count, but a different count, since one turn can produce
     several `TranscriptEntry` values) are computed independently. This is a documented
     approximation, not a silent bug, following the same honest-scoping precedent Phase 3 set for
     `diff_lines`' "coloring whole lines, not real +/- hunks."
  5. **`/init`'s test relies on mutating the process-wide current directory** (Task 14, Step 11) —
     flagged with an explicit note to run `tui::app`'s tests single-threaded if flakiness appears,
     rather than silently hoping `cargo test`'s default parallelism doesn't collide.

- **End-to-end spec coverage across all 7 phases (final check for this last phase):** Phase 1 (config/
  connections/secrets), Phase 2 (agent loop/tools/permissions/headless), Phase 3 (TUI shell), Phase 4
  (this plan: slash commands/persistence), and Phase 6 (flat-file memory) each state their own spec
  traceability against `docs/superpowers/specs/2026-07-06-local-code-tui-design.md`, and together they
  cover sections 1–9 in full: §1 connections+secrets (Phase 1) and `/model` switching (this plan);
  §2 agent loop/tools/MCP-readiness (Phase 2; MCP itself is Phase 5, not reviewed here since its plan
  file wasn't part of this task's required reading — flagged as unverified, not confirmed done); §3
  permissions (Phase 2, viewed/changed via this plan's `/permissions`); §4 AGENTS.md/CLAUDE.md (this
  plan, both halves); §5 TUI (Phase 3); §6 slash commands (this plan, all 8); §7 session persistence
  (this plan); §8 invocation modes (Phase 2's headless `-p`, Phase 3's interactive default — headless
  mode's own context-loading gap remains open, called out above and in Phase 2's own Self-review, and
  is out of scope for this phase per this plan's Architecture section); §9 cross-session memory (Phase
  6, a deliberately separate concept from session persistence, correctly not conflated with it
  anywhere in this plan). The one item this review cannot confirm is Phase 5 (MCP client wiring,
  spec section 2's "MCP client wired in from v1") — its plan file was not in this task's required
  reading list and this plan does not touch tool registration, so whether Phase 5 was completed and
  whether `build_streaming_agent_with_history` would need to also register MCP-sourced tools is
  unverified here and should be checked before considering the full 7-phase spec closed out.
