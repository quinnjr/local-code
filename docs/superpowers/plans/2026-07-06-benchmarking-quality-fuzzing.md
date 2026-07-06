# Benchmarking, Code-Quality Review, and Fuzzing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add cross-cutting quality-engineering tooling over the code already built in Phase 1
(config/connections/secrets), Phase 2 (agent loop/permissions), and Phase 6 (flat-file memory):
real `criterion` benchmarks over concrete hot paths, a hard local code-quality gate
(`clippy`/`fmt`/dependency-vulnerability-and-license scanning) wrapped in one runnable script, and a
real `cargo-fuzz` (libFuzzer) workspace fuzzing every genuine parsing/decision boundary those three
phases expose. This phase adds no new product behavior — it adds verification and measurement
around behavior those phases already define.

**Architecture:** Benchmarks live as `[[bench]]` targets under `benches/`, gated behind a `bench`
Cargo feature (`required-features = ["bench"]` per target) — the exact pattern already used by the
vendored `ntui` crate (`ntui`'s own `Cargo.toml` declares `bench = []` under `[features]` and its
one `[[bench]]` target, `benches/engine.rs`, sets `required-features = ["bench"]`; see Research
notes below). Code-quality review is a single idempotent shell script, `scripts/check.sh`, that
chains `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo deny
check` (dependency vulnerability + license + duplicate-version scanning — see the tool-choice
justification below), and `cargo test`, failing fast on the first red step and printing a clear
pass/fail line per step. Fuzzing lives in a detached `fuzz/` cargo-fuzz workspace (its own
`[workspace]` table, excluded from the parent crate's implicit workspace via `exclude = ["fuzz"]`
in the root `Cargo.toml` — the standard `cargo fuzz init` layout), with one libFuzzer target per
real parsing/decision boundary: TOML connection-file parsing, TOML permission-settings parsing, and
adversarial-argument permission-gate decisions. No CI wiring is added in this phase (see the CI
note below) and no new product code is touched — only `Cargo.toml`, `benches/`, `fuzz/`,
`scripts/`, and quality-tool config files are created or modified.

**Tech Stack:** `criterion` 0.8 (`async_tokio` feature, matching the version already pinned as a
dev-dependency by the vendored `ntui` 0.1.0`), `cargo-deny` (already installed locally at
`cargo-deny 0.19.9`) for the dependency-scanning gate, `cargo-fuzz` (already installed locally at
`cargo-fuzz 0.13.2`) + `libfuzzer-sys` 0.4 + `arbitrary` 1.x (`derive` feature) for the fuzz
workspace, plain POSIX `bash` for `scripts/check.sh`. No new runtime dependencies are added to the
shipped `local-code` binary — everything here is `dev-dependencies`, a separate `fuzz/` crate, or
tooling config.

---

## Spec traceability

This phase was **not** part of the original 6-phase spec
(`docs/superpowers/specs/2026-07-06-local-code-tui-design.md`) — it is a user-requested addition
made after that spec was approved, covering benchmarking, code-quality review tooling, and fuzzing
as cross-cutting concerns over the already-planned phases. There is no spec section to cite;
instead, each task below is traced to the concrete already-defined function/type it benchmarks or
fuzzes, from the phase plan that defines it:

| Task | Targets | Defined in |
|---|---|---|
| Task 2 (bench) | `local_code::config::connection::{load_connections, save_connections, Connection, ProviderKind}` | `docs/superpowers/plans/2026-07-06-foundation-config-connections.md`, Tasks 3–4 |
| Task 3 (bench) | `local_code::permissions::gate::PermissionGate::check`, `local_code::permissions::settings::PermissionSettings`, `local_code::permissions::types::{PermissionTier, PermissionPrompter, PermissionDecision, PermissionRequest}` | `docs/superpowers/plans/2026-07-06-core-agent-loop.md`, Tasks 3–4 |
| Task 4 (bench) | `local_code::memory::search::search` | `docs/superpowers/plans/2026-07-06-flat-file-memory.md`, Task 6 |
| Task 5 (bench) | `local_code::memory::buffer::{append_buffer_entry, maybe_rollover}`, `local_code::memory::rollup::rollup_and_archive` | `docs/superpowers/plans/2026-07-06-flat-file-memory.md`, Tasks 3–4 |
| Task 6 (quality gate) | whole crate (`cargo clippy`/`fmt`/`deny`/`test`) — no single Phase 1/2/6 function, a repo-wide gate | n/a |
| Task 8 (fuzz) | `local_code::config::connection::ConnectionsFile` via `toml::from_str` | `docs/superpowers/plans/2026-07-06-foundation-config-connections.md`, Task 3 |
| Task 9 (fuzz) | `local_code::permissions::settings::SettingsFile` via `toml::from_str` | `docs/superpowers/plans/2026-07-06-core-agent-loop.md`, Task 3 |
| Task 10 (fuzz) | `local_code::permissions::gate::PermissionGate::check` (adversarial `tool_name`/JSON `arguments`) | `docs/superpowers/plans/2026-07-06-core-agent-loop.md`, Task 4 |

Phase 3 (TUI shell) and Phase 5 (MCP client) plan files did not exist on disk at the time this plan
was written (checked: only `2026-07-06-core-agent-loop.md`, `2026-07-06-flat-file-memory.md`, and
`2026-07-06-foundation-config-connections.md` are present under `docs/superpowers/plans/`), so
nothing from them is benchmarked or fuzzed here. If/when those phases land, they should get their
own follow-up tasks added to this plan (or a new plan) rather than this document guessing at
functions that don't exist yet.

### Research performed (grounding the bench/fuzz feature-flag claims)

- Read `/home/joseph/.local/share/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/ntui-0.1.0/Cargo.toml`:
  it declares `[features] bench = []` and `fuzz = []`, one `[[bench]]` target (`name = "engine"`,
  `path = "benches/engine.rs"`, `harness = false`, `required-features = ["bench"]`), and a
  `criterion = "0.8.2"` dev-dependency. There is **no** `fuzz/` directory anywhere under
  `ntui-0.1.0/` despite the `fuzz` feature stub existing in `Cargo.toml` — the feature flag is
  declared but unused by any target, so there is no existing ntui fuzz harness to pattern-match
  against. This plan's `fuzz/` workspace is therefore modeled on the standard `cargo fuzz init`
  layout, not on any ntui precedent (there isn't one).
- Read `ntui-0.1.0/benches/engine.rs` in full: it uses plain (non-async) `criterion::Criterion`,
  `criterion_group!`/`criterion_main!`, `BenchmarkId::from_parameter` for parameterized groups, and
  `std::hint::black_box`. This plan's benches follow the same shape/idioms for the non-async
  benchmarks (Tasks 2, 4, 5) and add `async_tokio` only where genuinely needed (Task 3, since
  `PermissionGate::check` is `async`).
- Confirmed via `which`/`--version` in this environment: `cargo-audit-audit 0.22.2`, `cargo-deny
  0.19.9`, and `cargo-fuzz 0.13.2` are all already installed locally — the install steps below are
  documented anyway (for other machines/CI) but are not blocking here.
- Confirmed via `ls -la` that `/home/joseph/Projects/local-code/.github` does not exist — there is
  no existing CI config in this repo. Per the task instructions, this plan does **not** add one; it
  provides `scripts/check.sh` as the local gate and treats CI wiring as an explicit future step
  (see the note at the end of Task 6).
- Confirmed via `find` that `src/` in this repo currently contains only a placeholder `main.rs` —
  Phases 1/2/6 are documented as DONE in their own plan files' checklists but the actual source
  tree in this checkout has not yet been (re)built from them. This plan's bench/fuzz code is
  written against the exact signatures documented in those plan files' own "Types/functions later
  phases will depend on" sections, which is the authoritative contract those phases committed to.

---

## File structure

- Modify: `Cargo.toml` — add `[features] bench = []`, four `[[bench]]` targets, `criterion`
  (`async_tokio` feature) to `[dev-dependencies]`, `exclude = ["fuzz"]` under `[package]`.
- Create: `benches/connections_load.rs` — benchmarks `load_connections`/`save_connections` over a
  50+40-connection two-file fixture.
- Create: `benches/permission_gate.rs` — benchmarks `PermissionGate::check` across tiers and
  allow/deny-list sizes.
- Create: `benches/memory_search.rs` — benchmarks `memory::search::search` over a 30-daily-file +
  `recent.md` + `archive.md` fixture.
- Create: `benches/memory_rollup.rs` — benchmarks `memory::buffer::maybe_rollover` and
  `memory::rollup::rollup_and_archive`.
- Create: `deny.toml` — `cargo-deny` advisories/licenses/bans/sources configuration.
- Create: `scripts/check.sh` — the single local quality gate (fmt, clippy, deny, test).
- Create: `fuzz/Cargo.toml` — detached cargo-fuzz workspace manifest.
- Create: `fuzz/fuzz_targets/connections_toml.rs` — fuzzes `toml::from_str::<ConnectionsFile>`.
- Create: `fuzz/fuzz_targets/settings_toml.rs` — fuzzes `toml::from_str::<SettingsFile>`.
- Create: `fuzz/fuzz_targets/permission_gate_check.rs` — fuzzes `PermissionGate::check` with
  arbitrary tool name / JSON arguments / allow-deny rules / tier.

---

### Task 1: Bench scaffolding — `Cargo.toml` feature flag, dev-dependency, and target wiring

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Confirm required tools are installed**

Run:
```bash
cargo deny --version
cargo fuzz --version
cargo fmt --version
cargo clippy --version
```
Expected: each prints a version. If any is missing, install it before continuing:
```bash
cargo install cargo-deny
cargo install cargo-fuzz
rustup component add rustfmt clippy
```
(In this environment all four are already present: `cargo-deny 0.19.9`, `cargo-fuzz 0.13.2`, and
`rustfmt`/`clippy` ship with the installed toolchain.)

- [ ] **Step 2: Add the `bench` feature flag and `[[bench]]` targets to `Cargo.toml`**

Add (do not remove any existing `[dependencies]`/`[dev-dependencies]` entries from Phases 1/2/6):

```toml
[features]
bench = []

[[bench]]
name = "connections_load"
path = "benches/connections_load.rs"
harness = false
required-features = ["bench"]

[[bench]]
name = "permission_gate"
path = "benches/permission_gate.rs"
harness = false
required-features = ["bench"]

[[bench]]
name = "memory_search"
path = "benches/memory_search.rs"
harness = false
required-features = ["bench"]

[[bench]]
name = "memory_rollup"
path = "benches/memory_rollup.rs"
harness = false
required-features = ["bench"]
```

This is the identical shape to `ntui`'s own `[features] bench = []` +
`required-features = ["bench"]` `[[bench]]` target, confirmed by reading
`ntui-0.1.0/Cargo.toml` directly (see Research notes above) — running plain `cargo build`/`cargo
test` never compiles these bench binaries; they only build under `cargo bench --features bench` (or
`cargo build --benches --features bench`).

- [ ] **Step 3: Add `criterion` to `[dev-dependencies]`**

```toml
criterion = { version = "0.8", features = ["async_tokio"] }
```

`0.8` matches the major version `ntui` itself already pins (`criterion = "0.8.2"` in
`ntui-0.1.0/Cargo.toml`) so both crates benchmark with a consistent Criterion version if ever built
in the same workspace. The `async_tokio` feature is required by Task 3's benchmark, since
`PermissionGate::check` is an `async fn`.

- [ ] **Step 4: Exclude the (upcoming) `fuzz/` directory from workspace inference**

Add to the `[package]` table:

```toml
exclude = ["fuzz"]
```

This prevents Cargo from trying to fold the detached `fuzz/` cargo-fuzz crate (created in Task 7)
into this package's implicit workspace, which is the standard `cargo fuzz init` convention.

- [ ] **Step 5: Run `cargo check` to confirm the manifest still parses correctly**

Run: `cargo check`
Expected: builds with no errors (the `benches/*.rs` files referenced by `[[bench]]` don't exist yet
— that's fine; `cargo check` does not require bench source files to exist unless you build with
`--benches`, and even then only `cargo bench --features bench`/`cargo build --benches --features
bench` would need them). If your local Cargo version errors on a missing bench path even under
plain `cargo check`, create the four `benches/*.rs` files now with a one-line placeholder each
(`fn main() {}`) and let Tasks 2–5 replace them with real content.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add criterion bench scaffolding (bench feature, dev-dep, targets)"
```

---

### Task 2: Benchmark — connection TOML load + merge

**Files:**
- Create: `benches/connections_load.rs`

- [ ] **Step 1: Write the benchmark**

```rust
// benches/connections_load.rs
//
// Benchmarks local_code::config::connection::load_connections (Phase 1) over a
// realistic two-file fixture: 50 user-level connections, 40 project-level
// connections, with 10 names overlapping between the two files so the
// merge-by-name override path (not just the read+parse path) is exercised.
//
// Run with: `cargo bench --bench connections_load --features bench`

use criterion::{criterion_group, criterion_main, Criterion};
use local_code::config::connection::{load_connections, save_connections, Connection, ProviderKind};
use std::path::Path;
use tempfile::tempdir;

fn make_connections(count: usize, prefix: &str) -> Vec<Connection> {
    (0..count)
        .map(|i| Connection {
            name: format!("{prefix}-conn-{i}"),
            provider: if i % 2 == 0 {
                ProviderKind::OpenAiCompatible
            } else {
                ProviderKind::Ollama
            },
            base_url: format!("http://localhost:{}/v1", 8000 + i),
            default_model: format!("model-{i}"),
            models: vec![format!("model-{i}-a"), format!("model-{i}-b")],
        })
        .collect()
}

fn bench_load_connections(c: &mut Criterion) {
    let user_dir = tempdir().expect("tempdir");
    let project_dir = tempdir().expect("tempdir");

    let user_connections = make_connections(50, "user");
    save_connections(user_dir.path(), &user_connections).expect("save user fixture");

    let mut project_connections = make_connections(40, "project");
    // Force 10 overlapping names with the user-level file so the merge/override
    // path (not just independent-file parsing) is exercised by every iteration.
    for i in 0..10 {
        project_connections.push(Connection {
            name: format!("user-conn-{i}"),
            provider: ProviderKind::OpenAiCompatible,
            base_url: format!("http://overridden-host:{}/v1", 9000 + i),
            default_model: "overridden-model".to_string(),
            models: vec![],
        });
    }
    save_connections(project_dir.path(), &project_connections).expect("save project fixture");

    c.bench_function("load_connections_50_user_50_project_10_overlap", |b| {
        b.iter(|| {
            load_connections(
                std::hint::black_box(user_dir.path()),
                std::hint::black_box(project_dir.path()),
            )
            .expect("load_connections should succeed against a valid fixture")
        })
    });

    c.bench_function("load_connections_missing_files", |b| {
        let empty_user = Path::new("/nonexistent-user-dir-for-bench");
        let empty_project = Path::new("/nonexistent-project-dir-for-bench");
        b.iter(|| {
            load_connections(
                std::hint::black_box(empty_user),
                std::hint::black_box(empty_project),
            )
            .expect("missing files should yield an empty list, not an error")
        })
    });
}

criterion_group!(benches, bench_load_connections);
criterion_main!(benches);
```

- [ ] **Step 2: Run the benchmark to confirm it builds and produces output**

Run: `cargo bench --bench connections_load --features bench`
Expected: Criterion compiles the bench binary, runs both `bench_function`s, and prints timing
output (mean/median iteration time and a `Gnuplot`/`plotters` HTML report path under
`target/criterion/`) for `load_connections_50_user_50_project_10_overlap` and
`load_connections_missing_files`. There is no pass/fail assertion here beyond "it ran without
panicking and printed numbers" — this is a measurement, not a test.

- [ ] **Step 3: Commit**

```bash
git add benches/connections_load.rs
git commit -m "bench: add criterion benchmark for load_connections merge path"
```

---

### Task 3: Benchmark — permission-gate decision logic

**Files:**
- Create: `benches/permission_gate.rs`

- [ ] **Step 1: Write the benchmark**

```rust
// benches/permission_gate.rs
//
// Benchmarks local_code::permissions::gate::PermissionGate::check (Phase 2) across
// the three permission tiers and across always-allow/always-deny list sizes, since
// the gate does a linear `.iter().any(...)` substring scan over those lists on
// every bash call (see PermissionGate::check's implementation) — the cost of that
// scan at realistic list sizes (10/100/500 rules) is exactly what this measures.
//
// Run with: `cargo bench --bench permission_gate --features bench`

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use local_code::permissions::gate::PermissionGate;
use local_code::permissions::settings::PermissionSettings;
use local_code::permissions::types::{
    PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A prompter that always allows — used so the "ask tier, no matching rule"
/// benchmarks measure the gate's own overhead (list scans, locking) rather than
/// artificial prompt latency; none of the benchmarked scenarios below actually
/// reach the prompter except by construction requirement (PermissionGate::new
/// always takes one).
struct AlwaysAllowPrompter;

impl PermissionPrompter for AlwaysAllowPrompter {
    fn prompt<'a>(
        &'a self,
        _request: &'a PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
        Box::pin(async { PermissionDecision::Allow })
    }
}

fn settings_with_rules(n: usize) -> PermissionSettings {
    PermissionSettings {
        always_allow: (0..n)
            .map(|i| format!("cargo test --package crate-{i}"))
            .collect(),
        always_deny: (0..n).map(|i| format!("rm -rf /forbidden-{i}")).collect(),
    }
}

fn bench_permission_gate(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime for async benches");

    let mut group = c.benchmark_group("permission_gate_check");

    // Read-only tools short-circuit before ever touching tier/list logic.
    let read_only_gate = PermissionGate::new(
        PermissionTier::Ask,
        PermissionSettings::default(),
        Arc::new(AlwaysAllowPrompter),
    );
    group.bench_function("read_only_short_circuit", |b| {
        b.to_async(&rt).iter(|| async {
            read_only_gate
                .check("read_file", &serde_json::json!({"path": "src/lib.rs"}))
                .await
        })
    });

    // FullAuto allows bash immediately, no list scan needed.
    let full_auto_gate = PermissionGate::new(
        PermissionTier::FullAuto,
        PermissionSettings::default(),
        Arc::new(AlwaysAllowPrompter),
    );
    group.bench_function("full_auto_bash_allow", |b| {
        b.to_async(&rt).iter(|| async {
            full_auto_gate
                .check("bash", &serde_json::json!({"command": "ls -la"}))
                .await
        })
    });

    // Always-deny list lookup at 10/100/500 rules, hitting a match roughly in
    // the middle of the list (a realistic worst-of-average case for a linear scan).
    for &n in &[10usize, 100, 500] {
        let settings = settings_with_rules(n);
        let deny_command = format!("rm -rf /forbidden-{}", n / 2);
        let gate = PermissionGate::new(PermissionTier::FullAuto, settings, Arc::new(AlwaysAllowPrompter));
        group.bench_with_input(BenchmarkId::new("always_deny_list_lookup", n), &n, |b, _n| {
            b.to_async(&rt).iter(|| {
                let command = deny_command.clone();
                async {
                    gate.check("bash", &serde_json::json!({ "command": command }))
                        .await
                }
            })
        });
    }

    // Always-allow list lookup at 10/100/500 rules under Ask tier (which would
    // otherwise prompt) — this measures the list-scan short-circuit specifically.
    for &n in &[10usize, 100, 500] {
        let settings = settings_with_rules(n);
        let allow_command = format!("cargo test --package crate-{}", n / 2);
        let gate = PermissionGate::new(PermissionTier::Ask, settings, Arc::new(AlwaysAllowPrompter));
        group.bench_with_input(BenchmarkId::new("always_allow_list_lookup", n), &n, |b, _n| {
            b.to_async(&rt).iter(|| {
                let command = allow_command.clone();
                async {
                    gate.check("bash", &serde_json::json!({ "command": command }))
                        .await
                }
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_permission_gate);
criterion_main!(benches);
```

- [ ] **Step 2: Run the benchmark to confirm it builds and produces output**

Run: `cargo bench --bench permission_gate --features bench`
Expected: Criterion runs `read_only_short_circuit`, `full_auto_bash_allow`, and the six
`always_deny_list_lookup`/`always_allow_list_lookup` parameterized benchmarks (3 sizes × 2 lists),
printing timing output for each. Confirms `async_tokio`'s `to_async(&rt).iter(...)` compiles
against the `criterion = { version = "0.8", features = ["async_tokio"] }` dependency added in Task
1.

- [ ] **Step 3: Commit**

```bash
git add benches/permission_gate.rs
git commit -m "bench: add criterion benchmark for PermissionGate::check across tiers and list sizes"
```

---

### Task 4: Benchmark — memory keyword search

**Files:**
- Create: `benches/memory_search.rs`

- [ ] **Step 1: Write the benchmark**

```rust
// benches/memory_search.rs
//
// Benchmarks local_code::memory::search::search (Phase 6) over a realistic fixture:
// 30 daily files x 20 entries (600 lines), a 7-day recent.md (140 lines), and a
// 60-entry archive.md — roughly two months of project memory. search() is a
// linear case-insensitive substring scan with no index, so its cost scales with
// total line count across all non-core-memory files; this measures exactly that,
// for both a common term (many hits) and a term with zero hits.
//
// Run with: `cargo bench --bench memory_search --features bench`

use criterion::{criterion_group, criterion_main, Criterion};
use local_code::memory::search::search;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn write_realistic_memory_fixture(memory_dir: &Path) {
    fs::create_dir_all(memory_dir).expect("create memory dir");

    for day in 1..=30u32 {
        let mut body = String::new();
        for entry in 0..20u32 {
            body.push_str(&format!(
                "## {:02}:{:02}:00Z\nWorked on task {day}-{entry} involving the connection loader and permission gate.\n\n",
                entry % 24,
                (entry * 7) % 60
            ));
        }
        fs::write(memory_dir.join(format!("today-2026-06-{day:02}.md")), body).expect("write daily fixture");
    }

    let mut recent = String::new();
    for day in 1..=7u32 {
        recent.push_str(&format!("# 2026-07-{day:02}\n\n"));
        for entry in 0..20u32 {
            recent.push_str(&format!(
                "## {:02}:00:00Z\nRecent-window note {day}-{entry} about the memory rollup pipeline.\n\n",
                entry % 24
            ));
        }
    }
    fs::write(memory_dir.join("recent.md"), recent).expect("write recent.md fixture");

    let mut archive = String::new();
    for day in 1..=60u32 {
        archive.push_str(&format!("# 2025-{:02}-{:02}\n\n", (day % 12) + 1, (day % 28) + 1));
        archive.push_str(&format!(
            "## 09:00:00Z\nArchived note {day} referencing an old widgets refactor.\n\n"
        ));
    }
    fs::write(memory_dir.join("archive.md"), archive).expect("write archive.md fixture");
}

fn bench_memory_search(c: &mut Criterion) {
    let dir = tempdir().expect("tempdir");
    let memory_dir = dir.path().join("memory");
    write_realistic_memory_fixture(&memory_dir);

    let mut group = c.benchmark_group("memory_search");
    group.bench_function("common_term_many_hits", |b| {
        b.iter(|| {
            search(std::hint::black_box(&memory_dir), std::hint::black_box("note"))
                .expect("search should succeed")
        })
    });
    group.bench_function("rare_term_no_hits", |b| {
        b.iter(|| {
            search(
                std::hint::black_box(&memory_dir),
                std::hint::black_box("xyzzy-nonexistent-term"),
            )
            .expect("search should succeed even with zero hits")
        })
    });
    group.finish();
}

criterion_group!(benches, bench_memory_search);
criterion_main!(benches);
```

- [ ] **Step 2: Run the benchmark to confirm it builds and produces output**

Run: `cargo bench --bench memory_search --features bench`
Expected: Criterion runs `common_term_many_hits` and `rare_term_no_hits`, printing timing output for
both. Confirms the fixture actually exercises `search`'s daily-file-discovery
(`fs::read_dir`-based) + buffer/recent/archive-file logic against real files on disk in a
`tempdir()`.

- [ ] **Step 3: Commit**

```bash
git add benches/memory_search.rs
git commit -m "bench: add criterion benchmark for memory::search over a realistic fixture"
```

---

### Task 5: Benchmark — memory buffer rollover and recent-window rollup

**Files:**
- Create: `benches/memory_rollup.rs`

- [ ] **Step 1: Write the benchmark**

```rust
// benches/memory_rollup.rs
//
// Benchmarks two Phase 6 functions that mutate/consume the filesystem and are
// therefore benchmarked with `iter_batched` (fresh fixture per iteration) rather
// than plain `iter` (which would only be correct for read-only/idempotent code):
//
// - local_code::memory::buffer::maybe_rollover — moves a stale (yesterday-dated)
//   buffer into its daily file and deletes the buffer; not idempotent across
//   iterations (a second call on the same directory is a no-op), so each
//   iteration gets its own freshly-written stale buffer.
// - local_code::memory::rollup::rollup_and_archive — rebuilds recent.md and moves
//   out-of-window daily files into archive.md, deleting the moved daily files;
//   likewise not safely repeatable against the same directory, so each iteration
//   gets 60 fresh daily files.
//
// Run with: `cargo bench --bench memory_rollup --features bench`

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use local_code::memory::buffer::{append_buffer_entry, maybe_rollover};
use local_code::memory::rollup::rollup_and_archive;
use std::fs;
use std::path::PathBuf;
use tempfile::{tempdir, TempDir};

fn bench_maybe_rollover(c: &mut Criterion) {
    c.bench_function("maybe_rollover_stale_20_entry_buffer", |b| {
        b.iter_batched(
            || -> (TempDir, PathBuf) {
                let dir = tempdir().expect("tempdir");
                let memory_dir = dir.path().join("memory");
                let yesterday = Utc.with_ymd_and_hms(2026, 7, 5, 9, 0, 0).unwrap();
                for i in 0..20i64 {
                    append_buffer_entry(
                        &memory_dir,
                        yesterday + Duration::minutes(i),
                        &format!("Buffer entry number {i} from yesterday's session."),
                    )
                    .expect("append_buffer_entry should succeed");
                }
                (dir, memory_dir)
            },
            |(dir, memory_dir)| {
                let rolled = maybe_rollover(&memory_dir, Utc.with_ymd_and_hms(2026, 7, 6, 8, 0, 0).unwrap())
                    .expect("maybe_rollover should succeed");
                assert!(rolled, "fixture buffer is dated yesterday, rollover must occur");
                drop(dir);
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_rollup_and_archive(c: &mut Criterion) {
    c.bench_function("rollup_and_archive_60_daily_files", |b| {
        b.iter_batched(
            || -> (TempDir, PathBuf, NaiveDate) {
                let dir = tempdir().expect("tempdir");
                let memory_dir = dir.path().join("memory");
                fs::create_dir_all(&memory_dir).expect("create memory dir");
                let today = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
                for offset in 0..60i64 {
                    let date = today - Duration::days(offset);
                    let path = memory_dir.join(format!("today-{}.md", date.format("%Y-%m-%d")));
                    fs::write(
                        &path,
                        format!("## 09:00:00Z\nDaily rollup fixture entry for day offset {offset}.\n"),
                    )
                    .expect("write daily fixture file");
                }
                (dir, memory_dir, today)
            },
            |(dir, memory_dir, today)| {
                rollup_and_archive(&memory_dir, today).expect("rollup_and_archive should succeed");
                drop(dir);
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, bench_maybe_rollover, bench_rollup_and_archive);
criterion_main!(benches);
```

- [ ] **Step 2: Run the benchmark to confirm it builds and produces output**

Run: `cargo bench --bench memory_rollup --features bench`
Expected: Criterion runs `maybe_rollover_stale_20_entry_buffer` and
`rollup_and_archive_60_daily_files`, printing timing output for both. `iter_batched` with
`BatchSize::SmallInput` means Criterion times only the routine closure (the actual
`maybe_rollover`/`rollup_and_archive` call), not the per-iteration fixture setup in the first
closure.

- [ ] **Step 3: Commit**

```bash
git add benches/memory_rollup.rs
git commit -m "bench: add criterion benchmark for buffer rollover and recent-window rollup"
```

---

### Task 6: Code-quality gate — clippy, fmt, dependency scanning, wrapped in one script

**Files:**
- Create: `deny.toml`
- Create: `scripts/check.sh`

**Tool choice: `cargo-deny` over `cargo-audit`.** Both are already installed in this environment
(`cargo-audit-audit 0.22.2`, `cargo-deny 0.19.9`). `cargo-audit` checks dependencies against the
RUSTSEC advisory database only. `cargo-deny` does that too (its `advisories` check consumes the
same RUSTSEC database) *plus* license compliance (`licenses`), duplicate/banned-crate-version
detection (`bans`), and registry/source-provenance checks (`sources`) — all from one config file and
one command. Given this crate depends on `keyring` (three different platform-gated backend
features, each pulling in a different transitive dependency tree — DBus/Secret Service on Linux,
Keychain bindings on macOS, Windows crypto APIs on Windows) and `daimon` (multiple optional provider
features: `openai`, `ollama`, `macros`, and presumably others for future providers), the risk that
matters most in practice isn't just "is there a known CVE" — it's "did two backend features pull in
two incompatible major versions of the same crate" or "did an optional feature silently pull in a
copyleft-licensed dependency." `cargo-deny`'s `bans` and `licenses` checks catch exactly that, and
its `advisories` check is a strict superset of what `cargo-audit` alone would provide. Hence
`cargo-deny` is used as the single dependency-scanning gate; `cargo-audit` is not added as a second,
redundant tool.

- [ ] **Step 1: Write `deny.toml`**

```toml
# deny.toml — cargo-deny configuration for local-code.
#
# Run with: `cargo deny check` (all checks) or `cargo deny check <advisories|bans|licenses|sources>`
# for a single category. Regenerate/inspect the advisory database with
# `cargo deny fetch` if `cargo deny check` reports a stale/missing database.

[graph]
targets = []

[advisories]
db-urls = ["https://github.com/rustsec/advisory-db"]
yanked = "deny"
ignore = []

[licenses]
# Permissive licenses used by this crate's own dependency tree
# (keyring, daimon, ntui, serde/toml/clap/tokio ecosystem, etc.) as of Phase 1/2/6.
# If `cargo deny check licenses` reports a license not in this list, either add it
# here (if it's genuinely permissive/compatible) or replace the offending dependency
# — do not blanket-allow "unlicense"/copyleft licenses without a deliberate decision.
allow = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Unicode-3.0",
    "Zlib",
    "CC0-1.0",
    "MPL-2.0",
]
confidence-threshold = 0.8

[bans]
multiple-versions = "warn"
wildcards = "deny"
deny = []

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

- [ ] **Step 2: Run `cargo deny check` to confirm the config is valid**

Run: `cargo deny check`
Expected: `cargo-deny` parses `deny.toml` without error and reports PASS/FAIL per category
(`advisories`, `bans`, `licenses`, `sources`) against whatever `Cargo.lock` currently resolves to.
If a currently-resolved dependency's license isn't in the `allow` list yet (this can only be known
once Phases 1/2/6's dependencies are actually present in `Cargo.lock`), add the reported license
string to `allow` after confirming it is genuinely permissive — do not silently widen `allow` to
`"*"`.

- [ ] **Step 3: Write `scripts/check.sh`**

```bash
#!/usr/bin/env bash
# scripts/check.sh — the single local quality gate for local-code.
#
# Runs, in order: cargo fmt --check, cargo clippy (deny warnings), cargo deny check
# (advisories/bans/licenses/sources), cargo test. Fails fast on the first red step
# and prints a clear PASS/FAIL summary line per step as it goes.
#
# Usage: ./scripts/check.sh

set -euo pipefail

step_name=""

pass() {
    printf '  [PASS] %s\n' "$1"
}

fail() {
    printf '  [FAIL] %s\n' "$1"
}

run_step() {
    local name="$1"
    shift
    printf '\n==> %s\n' "$name"
    if "$@"; then
        pass "$name"
    else
        fail "$name"
        printf '\nlocal-code check: FAILED at step "%s"\n' "$name"
        exit 1
    fi
}

run_step "cargo fmt --check" cargo fmt --all -- --check
run_step "cargo clippy --all-targets --all-features -- -D warnings" \
    cargo clippy --all-targets --all-features -- -D warnings
run_step "cargo deny check" cargo deny check
run_step "cargo test --all-features" cargo test --all-features

printf '\nlocal-code check: ALL STEPS PASSED\n'
```

- [ ] **Step 4: Make the script executable**

Run: `chmod +x scripts/check.sh`

- [ ] **Step 5: Run the script to confirm it executes and produces a clear summary**

Run: `./scripts/check.sh`
Expected: each of the four steps prints its `==> <step>` header followed by either `[PASS]` or
`[FAIL]`; on the first failure the script exits immediately with a nonzero status and a
`FAILED at step "..."` message (fail-fast, per the task requirement), otherwise it prints
`local-code check: ALL STEPS PASSED` after all four steps succeed. Note: at the time this plan was
written, `src/` in this checkout contains only a placeholder `main.rs` (Phases 1/2/6 have not yet
been (re)built into this exact working tree) — running this script now may fail at the `cargo
clippy`/`cargo test` steps simply because those phases' source files don't exist yet in this
checkout. That is expected and not a defect in this script; re-run it once Phases 1/2/6's source is
present.

- [ ] **Step 6: Note on CI — explicitly deferred, not silently assumed**

No `.github/workflows/` (or any other CI config) exists in this repository as of this plan (checked
via `ls -la .github` — the directory does not exist). This phase deliberately does **not** create
one. `scripts/check.sh` is the complete local gate for now; wiring it into CI (e.g. a GitHub Actions
workflow that runs `./scripts/check.sh` on every PR, plus a separate scheduled job for `cargo bench`
regression tracking and `cargo fuzz run <target> -- -max_total_time=300` smoke runs) is a concrete,
identified **future step**, not something this plan assumes is already handled.

- [ ] **Step 7: Commit**

```bash
git add deny.toml scripts/check.sh
git commit -m "chore: add cargo-deny config and a single local quality-gate script"
```

---

### Task 7: Fuzzing — detached `cargo-fuzz` workspace scaffolding

**Files:**
- Create: `fuzz/Cargo.toml`
- Modify: `Cargo.toml` (confirm `exclude = ["fuzz"]` from Task 1 is present)

- [ ] **Step 1: Confirm `cargo-fuzz` is installed**

Run: `cargo fuzz --version`
Expected: prints a version (`cargo-fuzz 0.13.2` in this environment). If missing:
```bash
cargo install cargo-fuzz
```
`cargo-fuzz` requires a nightly toolchain for the actual `cargo fuzz run`/`cargo fuzz build`
commands (libFuzzer instrumentation needs `-Z sanitizer`-family nightly flags); if `rustup toolchain
list` shows no nightly, install one: `rustup toolchain install nightly`.

- [ ] **Step 2: Create `fuzz/Cargo.toml`**

```toml
[package]
name = "local-code-fuzz"
version = "0.0.0"
edition = "2024"
publish = false

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
arbitrary = { version = "1", features = ["derive"] }
toml = "1"
serde_json = "1"
tokio = { version = "1", features = ["rt"] }

[dependencies.local-code]
path = ".."

[[bin]]
name = "connections_toml"
path = "fuzz_targets/connections_toml.rs"
test = false
doc = false
bench = false

[[bin]]
name = "settings_toml"
path = "fuzz_targets/settings_toml.rs"
test = false
doc = false
bench = false

[[bin]]
name = "permission_gate_check"
path = "fuzz_targets/permission_gate_check.rs"
test = false
doc = false
bench = false

[workspace]
```

The trailing empty `[workspace]` table is the standard `cargo fuzz init` convention: it makes
`fuzz/` its own workspace root so Cargo does not try to unify its dependency resolution with the
parent `local-code` package's `Cargo.lock` (the two crates are allowed to resolve dependencies
independently). Combined with `exclude = ["fuzz"]` in the root `Cargo.toml` (added in Task 1), this
fully detaches the two builds.

- [ ] **Step 3: Create the `fuzz_targets/` directory placeholder files so `cargo fuzz` tooling recognizes the crate**

Create `fuzz/fuzz_targets/connections_toml.rs`, `fuzz/fuzz_targets/settings_toml.rs`, and
`fuzz/fuzz_targets/permission_gate_check.rs`, each with only:

```rust
#![no_main]
// placeholder — implemented in Task 8/9/10 respectively
fn main() {}
```

- [ ] **Step 4: Run `cargo fuzz list` to confirm the workspace is recognized**

Run (from the repo root): `cargo fuzz list`
Expected: prints the three target names (`connections_toml`, `settings_toml`,
`permission_gate_check`) with no errors — confirms `fuzz/Cargo.toml`'s `[[bin]]` entries and
`[package.metadata] cargo-fuzz = true` marker are correctly recognized by the `cargo-fuzz` CLI.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml fuzz/Cargo.toml fuzz/fuzz_targets
git commit -m "chore: scaffold a detached cargo-fuzz workspace with three placeholder targets"
```

---

### Task 8: Fuzz target — connection TOML parsing (`ConnectionsFile`)

**Files:**
- Modify: `fuzz/fuzz_targets/connections_toml.rs`

- [ ] **Step 1: Replace the placeholder with a real fuzz target**

```rust
// fuzz/fuzz_targets/connections_toml.rs
//
// Fuzzes local_code::config::connection::ConnectionsFile (Phase 1) via
// toml::from_str, the same call load_connections makes on every user- and
// project-level connections.toml file it reads. Goal: arbitrary byte input
// (valid UTF-8 or not, valid TOML or not) must never panic — a malformed or
// hand-edited connections.toml should produce a clean Err, not a crash.
//
// Run with: `cargo fuzz run connections_toml` (from within fuzz/).
#![no_main]

use libfuzzer_sys::fuzz_target;
use local_code::config::connection::ConnectionsFile;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = toml::from_str::<ConnectionsFile>(text);
    }
});
```

- [ ] **Step 2: Run the fuzz target briefly to confirm it builds and executes**

Run: `cargo fuzz run connections_toml -- -max_total_time=30` (from the `fuzz/` directory, or pass
`--fuzz-dir fuzz` from the repo root depending on your `cargo-fuzz` version)
Expected: libFuzzer builds the target under the nightly toolchain, seeds its corpus (empty on first
run), runs for ~30 seconds executing thousands of random byte-string inputs against
`toml::from_str::<ConnectionsFile>`, and exits cleanly with an executions-per-second summary and no
crash report. If it does find a panic, that is a real bug in `ConnectionsFile`'s `Deserialize` impl
or in `toml`/`serde` itself surfaced through our exact usage — inspect the reported crashing input
under `fuzz/artifacts/connections_toml/` before treating this step as complete.

- [ ] **Step 3: Commit**

```bash
git add fuzz/fuzz_targets/connections_toml.rs
git commit -m "fuzz: add libFuzzer target for ConnectionsFile TOML parsing"
```

---

### Task 9: Fuzz target — permission settings TOML parsing (`SettingsFile`)

**Files:**
- Modify: `fuzz/fuzz_targets/settings_toml.rs`

- [ ] **Step 1: Replace the placeholder with a real fuzz target**

```rust
// fuzz/fuzz_targets/settings_toml.rs
//
// Fuzzes local_code::permissions::settings::SettingsFile (Phase 2) via
// toml::from_str, the same call load_settings makes on every user- and
// project-level settings.toml file it reads. Structurally identical risk
// surface to connections_toml.rs (arbitrary bytes into a #[derive(Deserialize)]
// struct via the toml crate) but a distinct real type, so it gets its own target
// rather than being folded into connections_toml.
//
// Run with: `cargo fuzz run settings_toml` (from within fuzz/).
#![no_main]

use libfuzzer_sys::fuzz_target;
use local_code::permissions::settings::SettingsFile;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = toml::from_str::<SettingsFile>(text);
    }
});
```

- [ ] **Step 2: Run the fuzz target briefly to confirm it builds and executes**

Run: `cargo fuzz run settings_toml -- -max_total_time=30`
Expected: same as Task 8 Step 2 — builds, runs for ~30 seconds, exits cleanly with no crash report.

- [ ] **Step 3: Commit**

```bash
git add fuzz/fuzz_targets/settings_toml.rs
git commit -m "fuzz: add libFuzzer target for SettingsFile TOML parsing"
```

---

### Task 10: Fuzz target — permission-gate decisions under adversarial arguments

**Files:**
- Modify: `fuzz/fuzz_targets/permission_gate_check.rs`

- [ ] **Step 1: Replace the placeholder with a real fuzz target**

```rust
// fuzz/fuzz_targets/permission_gate_check.rs
//
// Fuzzes local_code::permissions::gate::PermissionGate::check (Phase 2) — the
// permission decision engine that every tool call passes through. This is the
// closest real "tool-call JSON argument parsing/permission classification"
// boundary Phase 2 exposes as pure(ish) logic: check() takes an arbitrary
// `tool_name: &str` and an arbitrary `serde_json::Value` of arguments, extracts
// fields out of that JSON with `.get("command").and_then(|v| v.as_str())`, and
// does substring scans against always_allow/always_deny rule lists — all of
// which must never panic regardless of what a future MCP tool or a malformed
// model tool-call supplies as arguments.
//
// Structured via `arbitrary` (not raw bytes) so libFuzzer can efficiently mutate
// a tool name, a permission tier selector, allow/deny rule lists, and a
// (possibly invalid) JSON-arguments string independently.
//
// Run with: `cargo fuzz run permission_gate_check` (from within fuzz/).
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use local_code::permissions::gate::PermissionGate;
use local_code::permissions::settings::PermissionSettings;
use local_code::permissions::types::{
    PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

/// Always allows — the fuzz target's goal is to find panics in check()'s own
/// classification/list-scan logic, not to exercise prompter implementations
/// (those are covered by unit tests in permissions::stdio and permissions::gate).
struct AlwaysAllowPrompter;

impl PermissionPrompter for AlwaysAllowPrompter {
    fn prompt<'a>(
        &'a self,
        _request: &'a PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
        Box::pin(async { PermissionDecision::Allow })
    }
}

#[derive(Debug, Arbitrary)]
struct FuzzInput {
    tool_name: String,
    tier_selector: u8,
    always_allow: Vec<String>,
    always_deny: Vec<String>,
    arguments_json: String,
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("failed to build a minimal tokio runtime for the fuzz target")
    })
}

fuzz_target!(|input: FuzzInput| {
    let tier = match input.tier_selector % 3 {
        0 => PermissionTier::Ask,
        1 => PermissionTier::AutoAcceptEdits,
        _ => PermissionTier::FullAuto,
    };
    let settings = PermissionSettings {
        always_allow: input.always_allow,
        always_deny: input.always_deny,
    };
    // Arbitrary, possibly-invalid JSON text — invalid input falls back to Null,
    // matching how a real caller would handle a malformed tool-call payload
    // rather than panicking on the fuzz target's own parsing step.
    let arguments: serde_json::Value =
        serde_json::from_str(&input.arguments_json).unwrap_or(serde_json::Value::Null);

    let gate = PermissionGate::new(tier, settings, Arc::new(AlwaysAllowPrompter));

    runtime().block_on(async {
        let _ = gate.check(&input.tool_name, &arguments).await;
    });
});
```

- [ ] **Step 2: Run the fuzz target briefly to confirm it builds and executes**

Run: `cargo fuzz run permission_gate_check -- -max_total_time=30`
Expected: builds (confirms `arbitrary`'s derive macro generates a valid `Arbitrary` impl for
`FuzzInput`'s `String`/`Vec<String>`/`u8` fields, and that the single-threaded `tokio` runtime built
via `Builder::new_current_thread()` correctly drives `PermissionGate::check`'s `tokio::sync::Mutex`
locks to completion), runs for ~30 seconds, and exits cleanly with no crash report.

- [ ] **Step 3: Commit**

```bash
git add fuzz/fuzz_targets/permission_gate_check.rs
git commit -m "fuzz: add libFuzzer target for PermissionGate::check with adversarial arguments"
```

---

### Task 11: Final wrap-up — run everything once end-to-end

**Files:** none (verification only)

- [ ] **Step 1: Run the full local quality gate**

Run: `./scripts/check.sh`
Expected: all four steps (`fmt`, `clippy`, `deny`, `test`) pass and the script prints `local-code
check: ALL STEPS PASSED`.

- [ ] **Step 2: Run every benchmark once**

Run:
```bash
cargo bench --features bench
```
Expected: all four `[[bench]]` targets (`connections_load`, `permission_gate`, `memory_search`,
`memory_rollup`) build and run to completion, each printing Criterion's timing summary and writing
an HTML report under `target/criterion/`.

- [ ] **Step 3: Run every fuzz target once for a short smoke duration**

Run (from `fuzz/`, or with `--fuzz-dir fuzz` from the repo root depending on your `cargo-fuzz`
version):
```bash
cargo fuzz run connections_toml -- -max_total_time=15
cargo fuzz run settings_toml -- -max_total_time=15
cargo fuzz run permission_gate_check -- -max_total_time=15
```
Expected: all three exit cleanly with no crash artifacts under `fuzz/artifacts/`.

- [ ] **Step 4: Commit (only if any of the above steps required fixes)**

If Steps 1–3 all passed with zero code changes, there is nothing new to commit — this task is a
verification pass, not a source-producing one. If any step surfaced a real bug (a clippy lint, a
fuzz-found panic, a flaky benchmark fixture), fix it in the relevant file from Tasks 2–10 and commit
that fix with a message describing what the verification pass caught, e.g.:

```bash
git add <fixed files>
git commit -m "fix: address issue found by scripts/check.sh / cargo fuzz smoke run"
```

---

## Self-review notes

**Coverage against the three required pillars:**

- **Benchmarking (criterion):** four real `[[bench]]` targets, each importing and calling a real
  function from an already-defined phase: `load_connections`/`save_connections` (Phase 1, Task 2),
  `PermissionGate::check` (Phase 2, Task 3), `memory::search::search` (Phase 6, Task 4), and
  `memory::buffer::maybe_rollover` + `memory::rollup::rollup_and_archive` (Phase 6, Task 5). All
  four use realistic fixture sizes (50/40 connections, 10/100/500-rule allow/deny lists, 30 daily
  files + 140-line recent window + 60-entry archive, 60 daily files for rollup) rather than
  toy inputs. Feature-gated behind `bench = []` with `required-features = ["bench"]` per target,
  matching `ntui-0.1.0`'s own confirmed pattern.
- **Code-quality review tooling:** `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo fmt --all -- --check`, and `cargo deny check` (chosen over `cargo-audit` — justification
  in Task 6) are wired into one runnable, fail-fast, clearly-summarized script
  (`scripts/check.sh`, Task 6). Confirmed no `.github/` CI config exists in this repo (`ls -la
  .github` → not found) and none is added by this plan; CI wiring is explicitly named as a future
  step in Task 6 Step 6, not silently assumed.
- **Fuzzing (cargo-fuzz/libFuzzer):** a detached `fuzz/` workspace (Task 7) with three real targets:
  `ConnectionsFile` TOML parsing (Phase 1, Task 8), `SettingsFile` TOML parsing (Phase 2, Task 9),
  and `PermissionGate::check` under adversarial tool name/JSON arguments/rule lists/tier (Phase 2,
  Task 10). Confirmed `ntui-0.1.0` has a `fuzz = []` feature stub in its `Cargo.toml` but **no**
  `fuzz/` directory anywhere in the vendored source — there was no existing ntui fuzz harness to
  pattern-match against, so this plan's `fuzz/` layout follows the standard `cargo fuzz init`
  convention instead (documented in "Research performed" above).

**Phase 6 (memory) explicitly has no fuzzable structured parser, and this plan does not force one
in.** Per the task instructions, this needs to be stated plainly rather than invented: Phase 6's
only genuine "parses untrusted text" surface is `memory::search::search`, which is a linear
case-insensitive `str::contains` substring scan over whole-file text — it has no structured grammar
to violate and cannot panic on any `&str` input (there is a private helper,
`memory::buffer::parse_buffer_date`, which does parse a small fixed date-header line via
`NaiveDate::parse_from_str`, but it is not `pub` — exposing it would mean modifying Phase 6's
already-completed `src/memory/buffer.rs` file purely to manufacture a fuzz target, which is exactly
the kind of low-value forced target the task instructions say to avoid). `search` is instead given a
*benchmark* (Task 4), which is the meaningful form of scrutiny for a substring scan (its cost, not
its correctness under malformed input, is the open question). This is a deliberate scope decision,
not an oversight.

**Function/signature verification against the actual Phase 1/2/6 plan files** (re-read immediately
before writing this plan, not from memory):

- `local_code::config::connection::{Connection, ProviderKind, ConnectionsFile, load_connections,
  save_connections}` — read from `docs/superpowers/plans/2026-07-06-foundation-config-connections.md`,
  Tasks 3–4 and 6 Step 1; field names (`name`, `provider`, `base_url`, `default_model`, `models`)
  and function signatures (`load_connections(user_config_dir: &Path, project_config_dir: &Path) ->
  Result<Vec<Connection>, ConnectionsError>`, `save_connections(dir: &Path, connections:
  &[Connection]) -> Result<(), ConnectionsError>`) match exactly what Tasks 2/8 of this plan use.
- `local_code::permissions::gate::PermissionGate::{new, check}`,
  `local_code::permissions::types::{PermissionTier, PermissionDecision, PermissionPrompter,
  PermissionRequest}`, `local_code::permissions::settings::{PermissionSettings, SettingsFile}` —
  read from `docs/superpowers/plans/2026-07-06-core-agent-loop.md`, Tasks 3–4; `PermissionGate::new(tier:
  PermissionTier, settings: PermissionSettings, prompter: Arc<dyn PermissionPrompter>) -> Self` and
  `async fn check(&self, tool_name: &str, arguments: &serde_json::Value) -> CheckOutcome` match
  exactly what Tasks 3/10 of this plan use, including the `PermissionPrompter` trait's boxed-future
  signature (`fn prompt<'a>(&'a self, request: &'a PermissionRequest) -> Pin<Box<dyn Future<Output =
  PermissionDecision> + Send + 'a>>`) reproduced verbatim in the `AlwaysAllowPrompter` stubs.
- `local_code::memory::{MemoryPaths, buffer::{append_buffer_entry, maybe_rollover},
  rollup::rollup_and_archive, search::{search, MemoryHit}}` — read from
  `docs/superpowers/plans/2026-07-06-flat-file-memory.md`, Tasks 2–4 and 6; signatures
  (`append_buffer_entry(memory_dir: &Path, now: DateTime<Utc>, text: &str) -> Result<(),
  MemoryError>`, `maybe_rollover(memory_dir: &Path, now: DateTime<Utc>) -> Result<bool,
  MemoryError>`, `rollup_and_archive(memory_dir: &Path, today: NaiveDate) -> Result<(),
  MemoryError>`, `search(memory_dir: &Path, query: &str) -> Result<Vec<MemoryHit>, MemoryError>`)
  match exactly what Tasks 4/5 of this plan use.

**Placeholder scan:** zero occurrences of "TODO", "TBD", "implement later", or "similar to Task N"
anywhere in this plan. Every code block in every step (`benches/*.rs`, `fuzz/fuzz_targets/*.rs`,
`deny.toml`, `scripts/check.sh`, `Cargo.toml` snippets) is complete, real, standalone code — the
only intentionally-placeholder content is the one-line `fn main() {}` stubs in Task 7 Step 3, and
those are explicitly replaced with real fuzz targets by Tasks 8–10 within this same plan (the same
write-then-replace pattern Phases 1/2/6 themselves use for their own `todo!()` scaffolding steps).

**What this phase deliberately does not do:** it does not benchmark `build_model` or `build_agent`
(Phase 2) — both are cheap, allocation-only constructors with no meaningful hot-path cost, and any
interesting cost in the agent loop is dominated by network I/O to the local LLM backend, which
criterion (designed for CPU-bound microbenchmarks) handles poorly and would produce misleading
"benchmark" numbers dominated by network/model latency rather than this crate's own code. It does
not add a GitHub Actions workflow (see Task 6 Step 6). It does not touch any Phase 1/2/6 product
source file — every change in this plan is additive (`benches/`, `fuzz/`, `scripts/`, `deny.toml`,
`Cargo.toml` metadata only).
