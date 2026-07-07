# Changelog

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
