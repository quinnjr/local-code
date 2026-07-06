# Local Code — TUI coding agent design

## Purpose
A Claude-Code-style, full-width TUI coding agent, but wired exclusively to local/local-network
OpenAI-compatible LLM APIs (llama.cpp server, vLLM, LM Studio, text-generation-webui, plus a
dedicated Ollama provider). Faithfully emulate the Claude Code agent loop: REPL + tools +
permission prompts + AGENTS.md/CLAUDE.md context + slash commands + session resume — scoped
strictly to coding tasks (read/write/edit files, run shell commands, search) with no
web-browsing/general-assistant scope creep.

## Crates
- `ntui` / `ntui-macros` — Ink-style TUI (Joseph's own crate), flexbox layout, components, hooks
- `daimon` (+ `daimon-core`, `daimon-macros`) — agent framework: ReAct loop, `Model` trait,
  `#[tool_fn]` macro, tool registry, MCP client, conversation memory
  - `openai` feature — generic OpenAI-compatible provider (`OpenAi::with_base_url(...)`)
  - `ollama` feature — dedicated native Ollama provider
  - `mcp` feature — MCP client (stdio/HTTP/WebSocket) for external tool servers
- `keyring` — OS secret manager (Keychain / Credential Manager / Secret Service via D-Bus)
- `directories` — XDG-on-Linux / `%APPDATA%`-on-Windows / `~/Library`-on-macOS config paths
- `clap` — CLI arg parsing for headless/scripted invocation
- `serde` + `toml` — connection/config file (de)serialization

## Architecture

### 1. Connections & secrets
- **Metadata** (name, provider type: `openai-compatible` | `ollama`, base_url, default model) lives
  in TOML, layered:
  - User-level: `directories::ProjectDirs` config dir (`~/.config/local-code/connections.toml` on
    Linux, `%APPDATA%\local-code\connections.toml` on Windows, etc.)
  - Project-level: `.local-code/connections.toml`, merged over user-level (project wins on name
    collision)
- **API keys** never touch the TOML file. Stored via `keyring` under service `local-code`,
  account = connection name. Connections with no key (many local servers need none) simply have
  none set — `keyring` lookup miss means "no auth header sent."
- **`/connections add`** wizard (also runs on first launch if zero connections exist): prompts for
  name → provider type → base_url → default model → optional API key → writes TOML + keyring.
- **`/model`**: lists all connections × their models (a connection can expose multiple models),
  lets you switch the active one mid-session. Conversation history carries over; only the
  model/connection used for subsequent turns changes — mirrors Claude Code's `/model`.

### 2. Agent loop
- Built on `daimon::Agent` (builder pattern), ReAct pattern: model call → tool_calls → execute →
  observe → repeat until a plain-text final response.
- **Tool-calling requirement**: v1 requires the connection's backend to support native
  OpenAI-style `tool_calls` in its chat completions API. No prompt-parsed ReAct fallback in v1
  (documented as a requirement; revisit later if demand shows up for text-completion-only
  backends).
- **Built-in tools** (v1), each a `#[tool_fn]`-annotated async fn registered by default:
  - `read_file`, `write_file`, `edit_file` (targeted find/replace, not whole-file overwrite)
  - `bash` (shell execution, subject to permission tier)
  - `grep` / `glob` search
- **MCP client** wired in from v1 (`daimon`'s `mcp` feature) so external tool servers can be
  attached via project/user config, same spirit as Claude Code's MCP support — this is the primary
  "make it easy to add more tooling" lever, alongside `#[tool_fn]` for first-party additions.

### 3. Permissions
- Tiered, Claude-Code-style:
  - **ask** (default): every write/edit/bash call prompts inline with numbered choices — Yes /
    Yes don't ask again this session / No + feedback.
  - **auto-accept-edits**: file writes/edits auto-approved, bash still prompts.
  - **full-auto**: nothing prompts (opt-in per session, e.g. a keybind or `--dangerously-skip-permissions`-equivalent flag).
- Project-level allow/deny list in `.local-code/settings.toml` (e.g. always-allow `cargo test`,
  always-deny `rm -rf`), layered under user-level settings the same way connections are.

### 4. Context loading (AGENTS.md / CLAUDE.md)
- At session start, load and concatenate into the system prompt, if present: project-level
  `AGENTS.md`, project-level `CLAUDE.md`, user-level equivalents (under the config dir) — same
  layering direction as connections/settings.
- **`/init`** analyzes the repo and generates/updates `AGENTS.md` only (never writes `CLAUDE.md` —
  that file is read for compatibility with existing Claude Code projects, not owned by this tool).

### 5. TUI (ntui)
- Full terminal width, single column, no sidebars — confirmed via mockup:
  - **Header bar**: active connection name, model, permission mode — always visible.
  - **Transcript**: user turns in a bordered box; assistant tool actions as inline collapsible
    cards (diffs rendered with +/- coloring for edits); permission prompts inline with numbered
    choices.
  - **Input box**: bottom, full width.
  - **Footer status line**: quick hints (`/model`, auto-accept toggle key) + token usage (cost
    tracking omitted for local models but the field stays for future non-local connections).
- Streaming: assistant text streams token-by-token into the transcript as it's generated
  (`generate_stream()` from `daimon`).

### 6. Slash commands (v1 set)
`/model`, `/connections` (add/list/remove), `/init`, `/permissions`, `/compact`, `/resume`,
`/clear`, `/help`.

### 7. Session persistence
- Sessions serialized to disk under the state dir (`directories`-resolved, e.g. XDG state dir on
  Linux), keyed by project path + timestamp.
- `local-code --resume` / `/resume` lists and reopens a prior session's transcript + connection.
- `/compact` summarizes older transcript turns via the active model when context grows large,
  same trigger-and-purpose as Claude Code's.

### 8. Invocation modes
- Interactive TUI REPL (default: `local-code`).
- Headless/scripted: `local-code -p "<prompt>"` runs one turn (agent loop to completion) and
  prints the final result, for CI/scripting use. There is no TTY to answer an inline prompt, so
  headless mode defaults its permission tier to **full-auto** (both edits and bash run
  unprompted); the project-level allow/deny list from section 3 still applies as a hard boundary.
  This default can be overridden with an explicit `--permission-mode` flag.

### 9. Cross-session memory
- Flat-file storage, not vector/RAG-backed, under the project's state dir (e.g.
  `.local-code/memory/`): a short-term buffer file, dated daily files, a rolled-up recent-window
  file, and an archive — same proven shape as this tool's own `.remember/` convention. Recalled by
  grep/keyword search on request, not embedding similarity.
- No embedding-model requirement placed on connections; storage stays human-readable and
  git-diffable, consistent with the AGENTS.md/CLAUDE.md plain-text philosophy.
- `sqlite-vector-rs` (HNSW-indexed vector types in SQLite, local-only, no network dependency) is
  the earmarked v2 upgrade if flat-file recall stops scaling — deferred until there's evidence
  it's needed, not built speculatively now.

## Out of scope for v1
- Prompt-based tool-calling fallback for non-native-tool-calling backends.
- Remote/cloud provider support (OpenAI proper, Anthropic, etc.) — explicitly local-only per the
  brief; the `Model` trait keeps the door open architecturally without building it now.
- Non-coding agent capabilities (web browsing, general chat assistant framing).
- Vector/RAG-backed memory (`sqlite-vector-rs`) — flat-file memory ships in v1; vector storage is
  the planned v2 upgrade.
