# Changelog

## Unreleased

- Added tmux-style workspace tabbing: one TUI can now host several agent
  sessions at once as **windows** (fullscreen tabs on a status bar) and
  **panes** (side-by-side or stacked splits), each with its own transcript,
  agent, permission state, and session file. `Ctrl+B` prefix chords, tmux
  defaults: `c` new window, `n`/`p`/`0-9` switch windows, `%`/`"` split,
  arrows/`o` move pane focus, `x` close pane (closing the last pane exits).
  Hidden windows keep streaming in the background; the tab bar marks busy
  windows with `✻`.

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
