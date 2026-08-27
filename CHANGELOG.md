# Changelog

## 0.2.0 — 2026-08-27

- Selectable reasoning effort: `/effort low|medium|high|off` in the TUI
  (a digit menu when bare), `--effort` on the CLI (TUI and `-p`), or a
  per-connection default via `effort = "high"` in `connections.toml`.
  The session override is shown in the dashboard, saved in the session
  file, and restored by `/resume`. Sent as the OpenAI-standard
  `reasoning_effort` field to openai-compatible connections (llama.cpp,
  vLLM, LM Studio); Ollama/OpenRouter connections report that it is
  remembered but not sent.
- Arch Linux packaging: `packaging/archlinux/PKGBUILD` builds the tagged
  release with `makepkg` (`dbus` runtime dep, `pass`/keyring backends as
  optdepends).
- The project is now dual-licensed under MIT OR Apache-2.0
  (`LICENSE-MIT`, `LICENSE-APACHE`).
- New `serve_artifacts` built-in tool: serves
  `<project>/.local-code/artifacts/` over a localhost-only HTTP server
  (OS-assigned port, unguessable token in the URL, reused across agent
  rebuilds) so the agent can showcase HTML/CSS/JS mockups and other
  visual work in your browser.
- The flat-file memory pipeline now actually runs: `memory add` rolls a
  previous day's buffer into its daily file and ages old daily files into
  `recent.md`/`archive.md`, and the new `memory core add <text>` records
  permanent core memories (all three functions existed but were never
  wired to a command).
- `C-b x` on a pane that is mid-turn now asks for a confirming second
  `C-b x` instead of silently discarding the in-flight turn.
- Transcript entries are stored behind `Arc`, so the per-keystroke and
  per-token renders bump refcounts instead of deep-copying the whole
  transcript; several smaller allocation/latency fixes (memory search,
  `/compact`, startup keyring read, workspace key handling).
- A failed agent rebuild during `/model`, `/resume`, or `/mcp add` now
  keeps the previous agent and reports the error instead of panicking.
- GitLab skill errors are no longer masked: an auth/rate-limit failure
  during `gl:` spec resolution reports the real HTTP error instead of
  "invalid skill source", and `skills list` renders GitLab sources
  without a stray leading slash.

- Added tmux-style workspace tabbing: one TUI can now host several agent
  sessions at once as **windows** (fullscreen tabs on a status bar) and
  **panes** (side-by-side or stacked splits), each with its own transcript,
  agent, permission state, and session file. `Ctrl+B` prefix chords, tmux
  defaults: `c` new window, `n`/`p`/`0-9` switch windows, `%`/`"` split,
  arrows/`o` move pane focus, `x` close pane (closing the last pane exits).
  Hidden windows keep streaming in the background; the tab bar marks busy
  windows with `✻`, and windows blocked on a permission decision with `!`
  (a background pane's prompt only accepts input once focused, so it is
  surfaced distinctly from ordinary streaming).
- New windows/panes create their session file eagerly, so `/resume` and
  `--resume` now hide sessions that never recorded a turn — a stack of
  opened-but-untouched tabs no longer clutters the resume list (the files
  themselves are kept and appear once they get their first turn).
- MCP server connection attempts are now bounded at 15s each; a stdio
  server that starts but never speaks the protocol previously blocked TUI
  startup forever.
- Perf: streamed assistant text no longer rewrites the whole transcript
  per token (was O(n²) copying over a long reply); per-turn session saves
  and `grep`/`glob` tree walks moved off the render thread; `grep` now
  skips files over the same 2 MiB cap `read_file` enforces.
- Session files now record the connection/model that is actually active
  (after `/model` or in-TUI `/resume`) instead of the one the process
  launched with, and save failures surface in the transcript instead of
  an invisible stderr line.

- **Breaking**: the MCP server config file was renamed from
  `mcp-servers.toml` to `mcp.toml`. If you have an existing
  `mcp-servers.toml` (project- or user-level), it's still read as a
  fallback when `mcp.toml` doesn't exist, so nothing breaks immediately —
  but rename it (`mv mcp-servers.toml mcp.toml`) at your convenience, since
  the fallback only reads the old name, it never migrates it automatically,
  and any future `/mcp add` or `mcp remove` will write a fresh `mcp.toml`
  alongside the untouched old file.
- Added a full `/mcp` command family (`list`/`remove`/`add`) — `/mcp add`
  is an in-TUI wizard supporting npm/pipx/custom-stdio/HTTP/SSE/WebSocket
  transports, with `Esc` to cancel and live agent reconnect on success.
- Added `${VAR_NAME}` environment-variable interpolation in `mcp.toml`, so
  secrets (API keys, tokens) don't need to be stored in the file itself.
- Added SSE and WebSocket MCP transports.

- **Breaking (Windows)**: per-user session state moved from the roaming
  profile (`%APPDATA%\local-code\local-code\data`) to the machine-local
  profile (`%LOCALAPPDATA%\local-code\local-code\data`). Existing sessions
  are migrated automatically on first run (one-time rename of the
  `sessions` dir when the new location doesn't exist yet); if migration
  can't happen — e.g. the two locations are on different volumes — move
  `%APPDATA%\local-code\local-code\data\sessions` to
  `%LOCALAPPDATA%\local-code\local-code\data\sessions` by hand to keep
  `--resume` history.
- Per-turn session saves are now crash-atomic: written to a temp file in
  the same directory, fsynced, then renamed into place — a crash or kill
  mid-turn can no longer corrupt a session file.
- CI now compile-checks all targets on windows-latest and macos-latest and
  runs the per-OS directory/session-store tests natively on those platforms.

## v0.1.0

Initial release.

- Config, connections, and OS-keyring-backed secret storage
- Core agent loop, permission tiers, and gated built-in tools
  (read/write/edit file, bash, grep, glob)
- Full-width terminal UI: streaming transcript, tool-call cards, inline
  permission prompts
- Slash commands: `/model`, `/permissions`, `/connections`, `/init`,
  `/compact`, `/resume`, `/clear`, `/help`
- Session persistence, including in-TUI and CLI (`--resume`) session resume
- MCP (Model Context Protocol) client support
- Flat-file cross-session memory (`memory search` / `memory core` / `memory add`)
- Headless mode (`local-code -p "..."`) for scripted, non-interactive use
