# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`local-code` is a Claude-Code-style terminal coding agent that talks exclusively to local and
local-network OpenAI-compatible LLM servers (llama.cpp, vLLM, LM Studio, Ollama) — no cloud calls,
no API keys required for inference. It's a Rust binary (`local-code`) built on top of two sibling
crates: `ntui` (a custom Ink-style TUI framework, flexbox layout via `taffy`) and `daimon` (the
agent framework providing `Agent`/`AgentBuilder`, model providers, MCP transports, tool-calling).
Both are external dependencies pulled from crates.io, not part of this repo.

## Workflow

This repo follows git-flow branching. `main` tracks production releases only; `develop` is the
integration branch. Do all work on a `feature/<name>` branch cut from `develop` — never commit
directly to `develop` or `main`. Land work via a PR from the feature branch into `develop`, not a
direct push. `release/<version>` branches cut from `develop` stage a release before it merges to
both `main` and back into `develop`; `hotfix/<name>` branches cut from `main` do the same for
urgent production fixes. If asked to start new work while sitting on `develop` or `main`, create
and switch to the appropriate branch first rather than committing in place.

Before merging a branch/PR (whether via subagent-driven-development, finishing-a-development-branch,
or any other workflow), stop and give the user the chance to run `/simplify`, `/code-review`,
`/optimize`, or similar audit commands themselves first — even if the branch already went through
its own review/fix loop. Present the branch as ready (tests green, reviews passed) and ask whether
to proceed with push/PR/merge, or wait while the user runs additional audits. Don't merge
automatically just because internal review passed.

## Commands

```bash
cargo build                          # debug build
cargo build --release                # release build
cargo test                           # full test suite (unit + integration, excludes live/ignored tests)
cargo test <module>::<test_name>     # single test, e.g. cargo test skills::spec::tests::gl_prefix
cargo test --lib skills::install     # all tests in one module
cargo test --test mcp_stdio_integration  # one integration test file under tests/
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
and `cargo test` as three parallel jobs on push/PR to `main`/`develop`. Run all three locally
before considering a change done — CI is the same three commands, not a superset.

### Live/ignored tests

`tests/live_*.rs` (`live_ollama`, `live_openai_compatible`, `live_compact`, `live_init`) require a
real running LLM server and are `#[ignore]`d by default. Run explicitly when needed:

```bash
cargo test --test live_ollama -- --ignored --nocapture
```

## Architecture

### Module map (`src/`)

- `cli/` — clap `Cli`/`Command` definitions and the non-interactive subcommand handlers
  (`connections`, `mcp`, `memory`, `skills`). `cli::run` is the top-level dispatcher called from
  `main.rs`: `-p/--prompt` routes to headless mode, a `Command` routes to a CLI subcommand handler,
  and no args/command launches the TUI.
- `tui/` — the interactive terminal UI, built as an `ntui` component tree.
  - `tui::run_tui` resolves the connection/model/settings/skills/MCP tools and hands off to
    `ntui::render(element!(App(...)))`.
  - `app.rs` is the single largest file in the crate (the root `App` component: turn loop, slash
    command dispatch, permission prompting, session persistence hooks).
  - `rebuild.rs` centralizes agent-rebuild logic (`/model`, `/resume`, `/mcp add` reconnect) so
    every rebuild site constructs an agent the same way.
  - `mcp_wizard.rs` is a pure state machine for the `/mcp add` in-TUI stepper (transport selection,
    prompts, `Advance` enum) — kept side-effect-free and unit-testable separately from `app.rs`'s
    wiring of it.
- `agent/` — wraps `daimon::agent::{Agent, AgentBuilder}` for this project's needs.
  - `build.rs::register_all_tools` is the **one and only tool-registration function** in the
    project — every built-in tool and every MCP-discovered tool passes through it, each wrapped in
    `GatedTool` (see `permissions/`), so permission enforcement is identical across all tool
    sources and both `Agent::prompt`/`Agent::prompt_stream`. Both TUI and headless mode call
    through this same function; don't add a parallel registration path.
  - `tools.rs` — the six built-in tools (`ReadFile`, `WriteFile`, `EditFile`, `Bash`, `Grep`,
    `Glob`).
  - `headless.rs` — the `-p/--prompt` non-interactive path (`run_headless`), used by both the CLI
    and by `local-code`'s own live integration tests.
- `permissions/` — the permission-tier system (`Ask` / `AutoAcceptEdits` / `FullAuto`).
  `gate::PermissionGate` is the single enforcement point every tool call passes through
  (`agent::gated_tool::GatedTool` wraps a tool and checks the gate before executing). `settings.rs`
  handles the persisted always-allow/always-deny lists; `stdio.rs` is the interactive terminal
  prompter used outside the TUI (headless mode, CLI wizards).
- `config/` — `Paths` (resolves user config/state dirs via `directories::ProjectDirs` plus the
  project-local `.local-code/` dir), `connection.rs` (LLM server connections), `mcp_servers.rs`
  (`mcp.toml` load/save with `${VAR}` env interpolation — see below), `secrets.rs` (OS
  keyring-backed API key storage via the `keyring` crate).
- `mcp/` — MCP (Model Context Protocol) client support. `connect.rs::connect_all` discovers tools
  from every configured server (stdio/HTTP/SSE/WebSocket transports, from `daimon`), tolerating
  individual server failures without aborting startup. `tool.rs::NamespacedMcpTool` wraps a
  discovered MCP tool so it can be registered like any other `Tool` via `register_all_tools`.
  `fixture_server.rs` is a hidden stdio MCP server the binary can turn itself into
  (`__mcp_fixture_server` arg, wired in `main.rs`) purely so integration tests can spawn a real
  child-process MCP server without a second `[[bin]]` target.
- `skills/` — downloadable skills (Claude-Code-style `SKILL.md` + supporting files), installable
  from GitHub, GitLab, or Bitbucket.
  - `spec.rs::parse_spec` is the unified entry point for all skill source specs: `gh:`/`gl:`/`bb:`
    prefixes, full URLs (github.com/gitlab.com/bitbucket.org), or a bare `owner/repo[/path][@ref]`
    (defaults to GitHub — this is the one required backward-compatibility guarantee across the
    whole module). GitLab shorthand specs can't be fully resolved synchronously (nested groups
    make the project-path/in-repo-path split ambiguous without an API call), so `parse_spec`
    returns a `ParsedSpec` with `needs_project_path_resolution: true` in that case; the caller runs
    `gitlab::GitlabClient::resolve_project_path` before proceeding.
  - `client.rs::SkillClient` is an enum (`GitHub`/`GitLab`/`Bitbucket`) wrapping the three concrete
    host clients (`github.rs`, `gitlab.rs`, `bitbucket.rs`) behind one set of async methods
    (`resolve_default_branch`, `resolve_commit_sha`, `fetch_directory_files`). This project uses
    **enum dispatch, not `dyn Trait`**, for this kind of "one of a few known variants" polymorphism
    — see also `config::mcp_servers::McpTransportConfig` for the same pattern. Follow it for any
    similar multi-backend addition rather than introducing a trait object.
  - `types.rs` holds shared cross-host types: `Host`, `SkillHostError`, `FetchedFile`,
    `SkillSource`, `InstalledSkillManifest` (`host` field is `#[serde(default)]` so manifests
    written before multi-host support still deserialize as `GitHub`).
  - `install.rs` is host-agnostic: `install_skill`/`update_skill` take `&SkillClient` and never
    branch on which host they're talking to — all host-specific behavior lives inside `SkillClient`
    and the three client modules.
  - `discovery.rs`/`frontmatter.rs`/`agent/skill_tool.rs` operate purely on already-downloaded
    local files and know nothing about hosts — everything upstream of "skill is on disk" is
    host-agnostic by design.
- `memory/` — flat-file, cross-session memory (`memory search`/`memory core`/`memory add`):
  `buffer.rs` (short-term), `rollup.rs` (daily/recent/archive rollup), `search.rs` (keyword search
  across all of it).
- `session/` — session persistence (`store.rs` load/save, `types.rs::SessionFile`); every TUI turn
  is saved so `local-code --resume` (or in-TUI `/resume`) can reopen it later.
- `context/mod.rs::load_project_context` — loads and concatenates project `AGENTS.md`/`CLAUDE.md`
  and user-level `AGENTS.md`/`CLAUDE.md` (in that order) into the system prompt. Both the TUI
  (`tui::run_tui`) and headless mode (`agent::headless::run_headless`) load this context.
- `init/` — the `/init` slash command's survey + generation logic for producing a project
  `CLAUDE.md`.

### Cross-cutting conventions

- **Enum dispatch over `dyn Trait`** for "one of N known concrete backends" polymorphism
  (`SkillClient`, `McpTransportConfig`). Keep following this pattern rather than introducing trait
  objects for similar future additions.
- **One tool-registration path.** Never add tools to an `Agent` outside
  `agent::build::register_all_tools` — TUI and headless mode must always end up with the same tool
  set built the same way.
- **`Paths`** (`config::paths::Paths`) is the single source of truth for where config/state live:
  `user_config_dir` (OS config dir via `directories`), `project_config_dir` (`.local-code/` under
  the project root), `user_state_dir` (OS state dir, sessions live here). Always resolve via
  `Paths::resolve(project_root)`, don't hand-roll path joins elsewhere.
- **`mcp.toml`** supports `${VAR_NAME}` interpolation from the environment at load time
  (`config::mcp_servers::load_mcp_servers` interpolates; `load_mcp_servers_raw` does not — used
  when round-tripping the file for editing so secrets aren't baked into what gets written back).
- **Secrets** are never stored in plaintext config files — `config::secrets::SecretStore` goes
  through the OS keyring (`keyring` crate, platform-specific backend per `Cargo.toml`'s
  `target.'cfg(...)'.dependencies`).
- Many modules that read/write via `stdin`/`stdout` (CLI wizards, `select_session_to_resume`, etc.)
  are generic over `BufRead`/`Write` specifically so they can be unit-tested without a real
  terminal — follow this pattern for any new interactive CLI flow.

See `TODO.md` for a list of currently-known, accepted v1 limitations (not bugs) — check it before
assuming something is broken rather than a documented trade-off.
