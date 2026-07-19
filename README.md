```
                                   __
                               _.-~  )
                    _..--~~~~,'   ,-/     _
                 .-'. . . .'   ,-','    ,' )
               ,'. . . _   ,--~,-'__..-'  ,'       __                 __                     __
             ,'. . .  (@)' ---~~~~      ,'        / /___  _________ _/ /     _________  ____/ /__
            /. . . . '~~             ,-'         / / __ \/ ___/ __ `/ /_____/ ___/ __ \/ __  / _ \
           /. . . . .             ,-'           / / /_/ / /__/ /_/ / /_____/ /__/ /_/ / /_/ /  __/
          ; . . . .  - .        ,'             /_/\____/\___/\__,_/_/      \___/\____/\__,_/\___/
         : . . . .       _     /
        . . . . .          `-.:
       . . . ./  - .          )
      .  . . |  _____..---.._/
 ~---~~~~----~~~~             ~~
```

A Claude-Code-style terminal coding agent that talks exclusively to local and
local-network OpenAI-compatible LLM servers — llama.cpp, vLLM, LM Studio, and
Ollama. No cloud calls, no API keys required.

## Features

- Full-width terminal UI with a streaming transcript, tool-call cards, and
  inline permission prompts
- tmux-style workspace tabbing: multiple concurrent agent sessions as
  windows (tabs) and split panes, driven by `Ctrl+B` prefix chords —
  `c` new window, `n`/`p`/`0-9` switch, `%`/`"` split, arrows/`o` move
  pane focus, `x` close. Background windows keep streaming (the tab bar
  marks them `✻`), and every pane is its own resumable session
- Slash commands: `/model`, `/permissions`, `/connections`, `/init`,
  `/compact`, `/resume`, `/clear`, `/help`
- Session persistence — every turn is saved, and sessions can be resumed
  in-TUI or via `local-code --resume`
- MCP (Model Context Protocol) client support (stdio/HTTP/SSE/WebSocket)
  alongside built-in file/shell tools, configurable via `/mcp add`'s in-TUI
  wizard or by hand-editing `mcp.toml` (see below)
- Flat-file cross-session memory (`memory search` / `memory core` /
  `memory core add` / `memory add`) with automatic daily rollover and
  recent/archive rollup
- GitHub-backed skills (`skills install` / `skills list` / `skills remove` / `skills update`)
- Headless mode (`local-code -p "..."`) for scripted, non-interactive use

## Configuring MCP servers (`mcp.toml`)

MCP servers are configured in `mcp.toml`, loaded from two places: the user-level
config dir (e.g. `~/.config/local-code/` on Linux) and the project-local
`.local-code/` dir. A project server replaces a user-level server with the same
name; otherwise both lists are merged. The easiest way to add one is the in-TUI
`/mcp add` wizard, which also captures an optional bearer token for HTTP/SSE
servers; the file can equally be edited by hand:

```toml
# stdio: spawn a child process and speak JSON-RPC over its stdin/stdout
[[server]]
name = "fs"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]

# http: POST each JSON-RPC message; headers are attached to every request
[[server]]
name = "github"
transport = "http"
url = "https://api.example.com/mcp"
[server.headers]
Authorization = "Bearer ${keyring:github-mcp}"

# sse: HTTP+SSE transport (persistent GET for responses, POSTs for requests)
[[server]]
name = "events"
transport = "sse"
url = "https://api.example.com/sse"

# websocket: persistent WebSocket; auth must be encoded in the URL itself
[[server]]
name = "ws-tools"
transport = "websocket"
url = "ws://localhost:9001/mcp?token=${WS_TOKEN}"
```

Every string field supports two kinds of secret references, resolved at load
time and never written back to the file:

- `${VAR_NAME}` — replaced with that environment variable's value.
- `${keyring:<name>}` — replaced with a secret from the OS keyring (Secret
  Service/libsecret on Linux, Keychain on macOS, Credential Manager on
  Windows). Store one with `local-code secret set <name>` (the value is
  prompted, so it never lands in shell history), list names with
  `local-code secret ls`, delete with `local-code secret rm <name>`. Secret
  names may contain letters, digits, `-` and `_`.

An unset variable or missing keyring entry resolves to an empty string, so a
misconfigured secret shows up as an auth failure from the server rather than
an error at startup.

## Secret storage

API keys and named secrets never live in plaintext files. They are stored in:

- **macOS** — the Keychain
- **Windows** — the Credential Manager
- **Linux and other unix** — the Secret Service (GNOME Keyring / KWallet).
  If no Secret Service daemon is running — headless boxes, minimal window
  managers, servers — local-code automatically falls back to
  [`pass`](https://www.passwordstore.org/), the standard unix password
  manager: entries are GPG-encrypted under `local-code/` in your password
  store (`$PASSWORD_STORE_DIR` or `~/.password-store`) and are fully
  readable with the `pass` CLI. The store must be initialized first
  (`pass init <gpg-id>`); building from source on unix requires libgpgme
  (`libgpgme-dev` on Debian/Ubuntu, `gpgme` on Arch).

The backend is chosen once per run, at the first secret access.

## Getting started

```bash
local-code connections add   # configure a local server (base URL + model)
local-code                   # launch the TUI
```

See `TODO.md` for known v1 limitations.
