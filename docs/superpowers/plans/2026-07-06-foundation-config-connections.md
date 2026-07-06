# Foundation: Config, Connections & Secrets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the config/connection/secret foundation that every later phase depends on: XDG/AppData-correct path resolution, layered (user + project) connection TOML files, OS-secret-manager-backed API key storage, and a `connections` CLI (add/list/remove) to manage them.

**Architecture:** Convert the crate from bin-only to lib+bin so the config/connection/secret logic is unit-testable in isolation from the TUI/agent loop built in later phases. `directories::ProjectDirs` resolves platform-correct config/state directories. Connection *metadata* is plain TOML, loaded from a user-level file and an optional project-level file and merged (project wins by connection name). Connection *secrets* (API keys) are never written to TOML — they go through a thin `SecretStore` wrapper around the `keyring` crate, keyed by connection name, testable via `keyring`'s built-in mock credential store. A `clap`-based CLI exposes `local-code connections add|list|remove`.

**Tech Stack:** Rust 2024 edition, `directories` 6.x, `keyring` 3.x (platform-gated features), `serde`/`toml`, `clap` (derive), `rpassword` (masked secret input), `thiserror` (typed errors), `anyhow` (top-level error context in `main`), `tempfile` (dev-dependency, test fixtures).

---

## Spec traceability

This plan implements spec section 1 ("Connections & secrets") from
`docs/superpowers/specs/2026-07-06-local-code-tui-design.md`, plus the `directories`/XDG
requirement from the Crates list. It does **not** implement the agent loop, TUI, slash commands,
MCP, or memory — those are later plans and depend on the types defined here:

- `local_code::config::paths::Paths` — resolved directories, used by every later phase to find
  config/state/memory locations.
- `local_code::config::connection::{Connection, ProviderKind, ConnectionsFile}` — the connection
  type the agent-loop phase will read to construct a `daimon` `Model`.
- `local_code::config::secrets::SecretStore` — the API key accessor the agent-loop phase will call
  before constructing a provider.

Later plans must import these exact names/signatures rather than redefining them.

---

## File structure

- Create: `Cargo.toml` (modify — add dependencies)
- Create: `src/lib.rs` — crate root, declares `pub mod config;` and `pub mod cli;`
- Create: `src/main.rs` (modify — becomes a thin entry point)
- Create: `src/config/mod.rs` — re-exports `paths`, `connection`, `secrets`
- Create: `src/config/paths.rs` — `Paths` resolution
- Create: `src/config/connection.rs` — `Connection`, `ProviderKind`, `ConnectionsFile`, load/merge
- Create: `src/config/secrets.rs` — `SecretStore`
- Create: `src/cli/mod.rs` — `Cli`, `Command`, `ConnectionsAction` (clap derive types) + dispatch
- Create: `src/cli/connections.rs` — `list`, `remove`, `add` (interactive wizard) implementations

---

### Task 1: Convert to lib+bin, add dependencies

**Files:**
- Modify: `Cargo.toml`
- Create: `src/lib.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add dependencies to `Cargo.toml`**

Replace the `[dependencies]` section:

```toml
[package]
name = "local-code"
version = "0.1.0"
edition = "2024"

[lib]
name = "local_code"
path = "src/lib.rs"

[[bin]]
name = "local-code"
path = "src/main.rs"

[dependencies]
ntui = "0.1.0"
ntui-macros = "0.1.0"
directories = "6.0"
serde = { version = "1", features = ["derive"] }
toml = "1"
clap = { version = "4", features = ["derive"] }
rpassword = "7"
thiserror = "2"
anyhow = "1"

[target.'cfg(target_os = "macos")'.dependencies]
keyring = { version = "3", features = ["apple-native"] }

[target.'cfg(target_os = "windows")'.dependencies]
keyring = { version = "3", features = ["windows-native"] }

[target.'cfg(target_os = "linux")'.dependencies]
keyring = { version = "3", features = ["sync-secret-service", "crypto-rust"] }

[dev-dependencies]
tempfile = "3"
```

`keyring` requires explicitly choosing a platform credential store feature per its own docs (it
has no default features) — hence the three `cfg`-gated blocks instead of one shared dependency
line. `sync-secret-service` + `crypto-rust` gives Linux a DBus Secret Service backend with
encrypted transport and no system OpenSSL requirement.

- [ ] **Step 2: Run `cargo check` to confirm dependencies resolve**

Run: `cargo check`
Expected: builds (with unused-code warnings, no errors) — confirms the platform-gated `keyring`
feature block matches your OS.

- [ ] **Step 3: Create `src/lib.rs`**

```rust
pub mod config;
pub mod cli;
```

- [ ] **Step 4: Create empty module files so the crate compiles**

Create `src/config/mod.rs`:

```rust
pub mod paths;
pub mod connection;
pub mod secrets;
```

Create `src/cli/mod.rs`:

```rust
pub mod connections;
```

- [ ] **Step 5: Replace `src/main.rs`**

```rust
fn main() {
    println!("local-code (foundation scaffold)");
}
```

- [ ] **Step 6: Run `cargo check` to confirm the lib+bin split compiles**

Run: `cargo check`
Expected: PASS, no errors.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/main.rs src/config/mod.rs src/cli/mod.rs
git commit -m "chore: split crate into lib+bin, add foundation dependencies"
```

---

### Task 2: Path resolution (`Paths`)

**Files:**
- Create: `src/config/paths.rs`
- Test: inline `#[cfg(test)] mod tests` in the same file

- [ ] **Step 1: Write the failing test**

```rust
// src/config/paths.rs

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Paths {
    pub user_config_dir: PathBuf,
    pub project_config_dir: PathBuf,
    pub user_state_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("could not determine the OS config/state directories for this platform")]
    NoProjectDirs,
}

impl Paths {
    pub fn resolve(project_root: &Path) -> Result<Paths, PathsError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_config_dir_is_dot_local_code_under_project_root() {
        let project_root = Path::new("/home/user/myproject");
        let paths = Paths::resolve(project_root).expect("resolve should succeed");
        assert_eq!(
            paths.project_config_dir,
            Path::new("/home/user/myproject/.local-code")
        );
    }

    #[test]
    fn user_config_dir_ends_with_local_code() {
        let project_root = Path::new("/home/user/myproject");
        let paths = Paths::resolve(project_root).expect("resolve should succeed");
        assert!(paths.user_config_dir.ends_with("local-code"));
    }

    #[test]
    fn user_state_dir_ends_with_local_code() {
        let project_root = Path::new("/home/user/myproject");
        let paths = Paths::resolve(project_root).expect("resolve should succeed");
        assert!(paths.user_state_dir.ends_with("local-code"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib config::paths`
Expected: FAIL (`not yet implemented` panic from `todo!()`)

- [ ] **Step 3: Implement `Paths::resolve`**

Replace the `todo!()` body:

```rust
impl Paths {
    pub fn resolve(project_root: &Path) -> Result<Paths, PathsError> {
        let project_dirs = directories::ProjectDirs::from("dev", "local-code", "local-code")
            .ok_or(PathsError::NoProjectDirs)?;

        let user_state_dir = project_dirs
            .state_dir()
            .unwrap_or_else(|| project_dirs.data_dir())
            .to_path_buf();

        Ok(Paths {
            user_config_dir: project_dirs.config_dir().to_path_buf(),
            project_config_dir: project_root.join(".local-code"),
            user_state_dir,
        })
    }
}
```

`ProjectDirs::state_dir()` only returns `Some` on Linux (XDG_STATE_HOME); macOS/Windows have no
separate state directory concept, so we fall back to `data_dir()` there.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib config::paths`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add src/config/paths.rs
git commit -m "feat: resolve user/project config and state directories"
```

---

### Task 3: Connection types and TOML (de)serialization

**Files:**
- Create: `src/config/connection.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/config/connection.rs

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OpenAiCompatible,
    Ollama,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub provider: ProviderKind,
    pub base_url: String,
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionsFile {
    #[serde(rename = "connection", default)]
    pub connections: Vec<Connection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML_FIXTURE: &str = r#"
[[connection]]
name = "local-vllm"
provider = "openai-compatible"
base_url = "http://localhost:8000/v1"
default_model = "qwen2.5-coder-32b"
models = ["qwen2.5-coder-32b", "llama-3.1-70b"]

[[connection]]
name = "home-ollama"
provider = "ollama"
base_url = "http://localhost:11434"
default_model = "llama3.1"
"#;

    #[test]
    fn parses_multiple_connections_from_toml() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        assert_eq!(file.connections.len(), 2);
        assert_eq!(file.connections[0].name, "local-vllm");
        assert_eq!(file.connections[0].provider, ProviderKind::OpenAiCompatible);
        assert_eq!(
            file.connections[0].models,
            vec!["qwen2.5-coder-32b", "llama-3.1-70b"]
        );
    }

    #[test]
    fn models_field_defaults_to_empty_when_omitted() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        assert_eq!(file.connections[1].name, "home-ollama");
        assert_eq!(file.connections[1].provider, ProviderKind::Ollama);
        assert!(file.connections[1].models.is_empty());
    }

    #[test]
    fn round_trips_through_serialization() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        let serialized = toml::to_string(&file).expect("serializes");
        let reparsed: ConnectionsFile = toml::from_str(&serialized).expect("reparses");
        assert_eq!(file, reparsed);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib config::connection`
Expected: FAIL to compile — `src/config/connection.rs` doesn't exist as a file yet in this repo
state (only the test module content above is written). Create the file with exactly the content
from Step 1 (types + tests together), then re-run; it should now compile and the assertions
should pass immediately since the types are plain data structs with derived (de)serialization.
If any assertion fails, fix the struct field names/serde attributes to match the fixture.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib config::connection`
Expected: PASS (3 tests)

- [ ] **Step 4: Commit**

```bash
git add src/config/connection.rs
git commit -m "feat: add Connection/ProviderKind/ConnectionsFile TOML schema"
```

---

### Task 4: Layered connection loading (user + project merge)

**Files:**
- Modify: `src/config/connection.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/config/connection.rs` (outside the existing `tests` module or inside it — add
these as new functions in the same `mod tests` block):

```rust
use std::fs;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionsError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

use std::path::PathBuf;

/// Loads and merges connections.toml from `user_config_dir` and `project_config_dir`.
/// A connection in the project file replaces a user-level connection of the same name;
/// otherwise entries from both files are kept, user-level first.
pub fn load_connections(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<Vec<Connection>, ConnectionsError> {
    let user_file = load_one(&user_config_dir.join("connections.toml"))?;
    let project_file = load_one(&project_config_dir.join("connections.toml"))?;

    let mut merged: Vec<Connection> = user_file.connections;
    for project_conn in project_file.connections {
        if let Some(existing) = merged.iter_mut().find(|c| c.name == project_conn.name) {
            *existing = project_conn;
        } else {
            merged.push(project_conn);
        }
    }
    Ok(merged)
}

fn load_one(path: &Path) -> Result<ConnectionsFile, ConnectionsError> {
    if !path.exists() {
        return Ok(ConnectionsFile::default());
    }
    let text = fs::read_to_string(path).map_err(|source| ConnectionsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ConnectionsError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("connections.toml"), contents).unwrap();
    }

    #[test]
    fn project_connection_overrides_user_connection_of_same_name() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        write(
            user_dir.path(),
            r#"
[[connection]]
name = "shared"
provider = "openai-compatible"
base_url = "http://user-host:8000/v1"
default_model = "model-a"
"#,
        );
        write(
            project_dir.path(),
            r#"
[[connection]]
name = "shared"
provider = "openai-compatible"
base_url = "http://project-host:8000/v1"
default_model = "model-b"
"#,
        );

        let connections = load_connections(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].base_url, "http://project-host:8000/v1");
        assert_eq!(connections[0].default_model, "model-b");
    }

    #[test]
    fn distinct_names_from_both_files_are_kept() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        write(
            user_dir.path(),
            r#"
[[connection]]
name = "user-conn"
provider = "openai-compatible"
base_url = "http://a/v1"
default_model = "m"
"#,
        );
        write(
            project_dir.path(),
            r#"
[[connection]]
name = "project-conn"
provider = "ollama"
base_url = "http://b"
default_model = "m2"
"#,
        );

        let connections = load_connections(user_dir.path(), project_dir.path()).unwrap();
        let names: Vec<_> = connections.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["user-conn", "project-conn"]);
    }

    #[test]
    fn missing_files_yield_empty_list_not_error() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let connections = load_connections(user_dir.path(), project_dir.path()).unwrap();
        assert!(connections.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib config::connection`
Expected: FAIL to compile initially (functions referenced before defined in the same edit) — this
is expected since Step 1's content *is* the implementation plus tests together. After adding it,
compile errors should resolve; run again and confirm the three new `merge_tests` pass.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib config::connection`
Expected: PASS (6 tests total: 3 from Task 3 + 3 from this task)

- [ ] **Step 4: Commit**

```bash
git add src/config/connection.rs
git commit -m "feat: load and merge user/project connections.toml"
```

---

### Task 5: Secret storage (`SecretStore`)

**Files:**
- Create: `src/config/secrets.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/config/secrets.rs

const SERVICE_NAME: &str = "local-code";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keyring operation failed: {0}")]
    Keyring(#[from] keyring::Error),
}

pub struct SecretStore;

impl SecretStore {
    /// Returns `Ok(None)` if no key is stored for this connection (e.g. the local
    /// server needs no auth), `Ok(Some(key))` if one is, and `Err` on a genuine
    /// backend failure.
    pub fn get_api_key(connection_name: &str) -> Result<Option<String>, SecretsError> {
        let entry = keyring::Entry::new(SERVICE_NAME, connection_name)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    pub fn set_api_key(connection_name: &str, api_key: &str) -> Result<(), SecretsError> {
        let entry = keyring::Entry::new(SERVICE_NAME, connection_name)?;
        entry.set_password(api_key)?;
        Ok(())
    }

    pub fn delete_api_key(connection_name: &str) -> Result<(), SecretsError> {
        let entry = keyring::Entry::new(SERVICE_NAME, connection_name)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(other) => Err(other.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    /// Switches keyring to its platform-independent in-memory mock store so tests
    /// don't touch the real OS secret manager and can run in CI without a display
    /// server / Keychain prompt.
    fn use_mock_keyring() {
        INIT.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    #[test]
    fn missing_key_returns_none_not_error() {
        use_mock_keyring();
        let result = SecretStore::get_api_key("conn-with-no-key-set-yet").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn set_then_get_round_trips() {
        use_mock_keyring();
        SecretStore::set_api_key("conn-a", "sk-test-123").unwrap();
        let result = SecretStore::get_api_key("conn-a").unwrap();
        assert_eq!(result, Some("sk-test-123".to_string()));
    }

    #[test]
    fn delete_removes_key() {
        use_mock_keyring();
        SecretStore::set_api_key("conn-b", "sk-test-456").unwrap();
        SecretStore::delete_api_key("conn-b").unwrap();
        let result = SecretStore::get_api_key("conn-b").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn delete_on_missing_key_is_not_an_error() {
        use_mock_keyring();
        SecretStore::delete_api_key("conn-never-existed").unwrap();
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib config::secrets`
Expected: FAIL to compile — file doesn't exist yet. Create it with the full content above.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib config::secrets`
Expected: PASS (4 tests). Note: because the mock credential builder is process-global
(`set_default_credential_builder`) and tests share one process, use distinct connection names per
test (already done above) to avoid cross-test interference — Rust runs tests in threads within
the same process by default.

- [ ] **Step 4: Commit**

```bash
git add src/config/secrets.rs
git commit -m "feat: store/retrieve/delete connection API keys via OS keyring"
```

---

### Task 6: `connections list` and `connections remove` CLI

**Files:**
- Create: `src/cli/connections.rs`
- Modify: `src/config/connection.rs` (add a `save_project_connections` helper used by `remove`)

- [ ] **Step 1: Add a save-back helper and its failing test to `src/config/connection.rs`**

Append inside the file (implementation) and inside `mod merge_tests` (test):

```rust
/// Overwrites the project-level connections.toml with exactly `connections`.
/// Used by `connections remove` — removal always targets the project-level file
/// since that's the file this CLI writes to (user-level file is hand-edited or
/// written by `connections add` when the user chooses to save it there).
pub fn save_connections(
    dir: &Path,
    connections: &[Connection],
) -> Result<(), ConnectionsError> {
    fs::create_dir_all(dir).map_err(|source| ConnectionsError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    let file = ConnectionsFile {
        connections: connections.to_vec(),
    };
    let text = toml::to_string_pretty(&file).expect("Connection serializes without error");
    fs::write(dir.join("connections.toml"), text).map_err(|source| ConnectionsError::Read {
        path: dir.to_path_buf(),
        source,
    })
}
```

Test (add to `mod merge_tests`):

```rust
    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let conn = Connection {
            name: "roundtrip".into(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:8000/v1".into(),
            default_model: "m".into(),
            models: vec![],
        };
        save_connections(dir.path(), &[conn.clone()]).unwrap();
        let loaded = load_connections(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(loaded, vec![conn]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib config::connection`
Expected: first FAIL (function doesn't exist) if added test-only; since both are added together,
expect PASS after adding (7 tests total). If it fails, check that `Connection` derives `Clone`
(it already does from Task 3).

- [ ] **Step 3: Commit**

```bash
git add src/config/connection.rs
git commit -m "feat: save project-level connections.toml"
```

- [ ] **Step 4: Write `src/cli/connections.rs` with `list` and `remove`**

```rust
use crate::config::connection::{load_connections, save_connections, Connection};
use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use std::io::Write;

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let connections =
        load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    if connections.is_empty() {
        writeln!(out, "No connections configured. Run `local-code connections add`.")?;
        return Ok(());
    }
    for conn in &connections {
        let has_key = SecretStore::get_api_key(&conn.name)?.is_some();
        writeln!(
            out,
            "{}  [{:?}]  {}  (default model: {}){}",
            conn.name,
            conn.provider,
            conn.base_url,
            conn.default_model,
            if has_key { "  [key stored]" } else { "" }
        )?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, mut out: W) -> anyhow::Result<()> {
    let mut connections =
        load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    let before = connections.len();
    connections.retain(|c| c.name != name);
    if connections.len() == before {
        writeln!(out, "No connection named '{name}' found.")?;
        return Ok(());
    }
    save_connections(&paths.project_config_dir, &connections)?;
    SecretStore::delete_api_key(name)?;
    writeln!(out, "Removed connection '{name}'.")?;
    Ok(())
}
```

- [ ] **Step 5: Write the failing tests for `list`/`remove`**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::connection::ProviderKind;
    use std::sync::Once;
    use tempfile::tempdir;

    static INIT: Once = Once::new();
    fn use_mock_keyring() {
        INIT.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    fn test_paths(project_dir: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: project_dir.join("user-config-unused"),
            project_config_dir: project_dir.to_path_buf(),
            user_state_dir: project_dir.join("state-unused"),
        }
    }

    #[test]
    fn list_reports_no_connections_when_empty() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No connections configured"));
    }

    #[test]
    fn list_prints_each_connection() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_connections(
            &paths.project_config_dir,
            &[Connection {
                name: "conn-x".into(),
                provider: ProviderKind::OpenAiCompatible,
                base_url: "http://localhost:8000/v1".into(),
                default_model: "m".into(),
                models: vec![],
            }],
        )
        .unwrap();

        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("conn-x"));
        assert!(printed.contains("http://localhost:8000/v1"));
    }

    #[test]
    fn remove_deletes_matching_connection_and_its_key() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_connections(
            &paths.project_config_dir,
            &[Connection {
                name: "conn-y".into(),
                provider: ProviderKind::Ollama,
                base_url: "http://localhost:11434".into(),
                default_model: "llama3.1".into(),
                models: vec![],
            }],
        )
        .unwrap();
        SecretStore::set_api_key("conn-y", "unused-key").unwrap();

        let mut out = Vec::new();
        remove(&paths, "conn-y", &mut out).unwrap();

        let remaining = load_connections(&paths.user_config_dir, &paths.project_config_dir).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(SecretStore::get_api_key("conn-y").unwrap(), None);
    }

    #[test]
    fn remove_reports_when_name_not_found() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let mut out = Vec::new();
        remove(&paths, "does-not-exist", &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No connection named"));
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib cli::connections`
Expected: PASS (4 tests)

- [ ] **Step 7: Commit**

```bash
git add src/cli/connections.rs
git commit -m "feat: add connections list/remove commands"
```

---

### Task 7: `connections add` interactive wizard

**Files:**
- Modify: `src/cli/connections.rs`

- [ ] **Step 1: Write the failing test**

The wizard is written generically over `BufRead`/`Write` so tests can supply an in-memory
transcript instead of a real terminal. Append to `src/cli/connections.rs` (implementation) and
its test module:

```rust
use crate::config::connection::ProviderKind;
use std::io::BufRead;

pub fn add<R: BufRead, W: Write>(
    paths: &Paths,
    mut input: R,
    mut out: W,
) -> anyhow::Result<Connection> {
    write!(out, "Connection name: ")?;
    out.flush()?;
    let name = read_line(&mut input)?;

    write!(out, "Provider type (1=openai-compatible, 2=ollama): ")?;
    out.flush()?;
    let provider = match read_line(&mut input)?.trim() {
        "2" => ProviderKind::Ollama,
        _ => ProviderKind::OpenAiCompatible,
    };

    write!(out, "Base URL: ")?;
    out.flush()?;
    let base_url = read_line(&mut input)?;

    write!(out, "Default model: ")?;
    out.flush()?;
    let default_model = read_line(&mut input)?;

    write!(out, "API key (leave blank if none): ")?;
    out.flush()?;
    let api_key = read_line(&mut input)?;

    let connection = Connection {
        name,
        provider,
        base_url,
        default_model,
        models: vec![],
    };

    let mut connections =
        load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    connections.retain(|c| c.name != connection.name);
    connections.push(connection.clone());
    save_connections(&paths.project_config_dir, &connections)?;

    if !api_key.is_empty() {
        SecretStore::set_api_key(&connection.name, &api_key)?;
    }

    writeln!(out, "Saved connection '{}'.", connection.name)?;
    Ok(connection)
}

fn read_line<R: BufRead>(input: &mut R) -> anyhow::Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_string())
}
```

Test (append to the existing `mod tests`):

```rust
    #[test]
    fn add_writes_connection_and_key_from_transcript() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let transcript = "local-vllm\n1\nhttp://localhost:8000/v1\nqwen2.5-coder-32b\nsk-test-789\n";
        let mut out = Vec::new();
        let connection = add(&paths, transcript.as_bytes(), &mut out).unwrap();

        assert_eq!(connection.name, "local-vllm");
        assert_eq!(connection.provider, ProviderKind::OpenAiCompatible);
        assert_eq!(connection.base_url, "http://localhost:8000/v1");
        assert_eq!(connection.default_model, "qwen2.5-coder-32b");

        let saved = load_connections(&paths.user_config_dir, &paths.project_config_dir).unwrap();
        assert_eq!(saved, vec![connection.clone()]);
        assert_eq!(
            SecretStore::get_api_key(&connection.name).unwrap(),
            Some("sk-test-789".to_string())
        );
    }

    #[test]
    fn add_with_blank_api_key_stores_no_secret() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let transcript = "home-ollama\n2\nhttp://localhost:11434\nllama3.1\n\n";
        let mut out = Vec::new();
        let connection = add(&paths, transcript.as_bytes(), &mut out).unwrap();

        assert_eq!(connection.provider, ProviderKind::Ollama);
        assert_eq!(
            SecretStore::get_api_key(&connection.name).unwrap(),
            None
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib cli::connections`
Expected: PASS (6 tests total: 4 from Task 6 + 2 new)

- [ ] **Step 3: Commit**

```bash
git add src/cli/connections.rs
git commit -m "feat: add interactive connections wizard"
```

---

### Task 8: Wire the `clap` CLI in `main.rs`

**Files:**
- Create: `src/cli/mod.rs` (modify — add `Cli`/`Command` types)
- Modify: `src/main.rs`

- [ ] **Step 1: Replace `src/cli/mod.rs`**

```rust
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
```

- [ ] **Step 2: Replace `src/main.rs`**

```rust
use clap::Parser;
use local_code::cli::{run, Cli};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let project_root = std::env::current_dir()?;
    run(cli, project_root)
}
```

- [ ] **Step 3: Run a full build and the full test suite**

Run: `cargo build && cargo test`
Expected: build succeeds; all previously-written tests still PASS.

- [ ] **Step 4: Manually verify the CLI end-to-end**

Run: `cargo run -- connections list`
Expected output: `No connections configured. Run \`local-code connections add\`.`

Run:
```bash
printf 'demo\n1\nhttp://localhost:8000/v1\ndemo-model\n\n' | cargo run -- connections add
cargo run -- connections list
cargo run -- connections remove demo
cargo run -- connections list
```
Expected: `add` prints `Saved connection 'demo'.`, `list` then shows the `demo` entry with base
URL and default model, `remove` prints `Removed connection 'demo'.`, and the final `list` reports
no connections again. This exercises the real (non-mocked) `keyring` backend on your machine — if
it prompts for OS-level permission (e.g. macOS Keychain access, Linux Secret Service unlock),
that's expected the first time.

- [ ] **Step 5: Commit**

```bash
git add src/cli/mod.rs src/main.rs
git commit -m "feat: wire clap CLI with connections add/list/remove"
```

---

## Self-review notes

- **Spec coverage:** connection metadata layering (user+project TOML), API keys never in TOML
  (keyring-backed), `/connections add`-equivalent wizard, XDG/AppData path resolution — all
  covered. `/model` switching and multi-model-per-connection *usage* are out of scope here (that's
  the agent-loop/TUI phases); this plan only stores the `models: Vec<String>` data they'll read.
- **No placeholders:** the one `todo!()` (Task 2, Step 1) is deliberately part of the
  write-failing-test step and is replaced in Step 3 of the same task — by the end of the plan
  every function has a real body.
- **Type consistency:** `Connection`, `ProviderKind`, `ConnectionsFile`, `Paths`, `SecretStore`,
  and `ConnectionsError`/`PathsError`/`SecretsError` are each defined exactly once and reused
  verbatim across later tasks in this file. Later plans (agent loop, TUI) must import these from
  `local_code::config::{paths, connection, secrets}` rather than redefining them.
