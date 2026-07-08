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
- Slash commands: `/model`, `/permissions`, `/connections`, `/init`,
  `/compact`, `/resume`, `/clear`, `/help`
- Session persistence — every turn is saved, and sessions can be resumed
  in-TUI or via `local-code --resume`
- MCP (Model Context Protocol) client support (stdio/HTTP/SSE/WebSocket)
  alongside built-in file/shell tools, configurable via `/mcp add`'s in-TUI
  wizard or by hand-editing `mcp.toml` — `${VAR_NAME}` references in
  `mcp.toml` are expanded from the environment at load time, so secrets
  (API keys, tokens) don't need to be stored in the file itself
- Flat-file cross-session memory (`memory search` / `memory core` / `memory add`)
- Headless mode (`local-code -p "..."`) for scripted, non-interactive use

## Getting started

```bash
local-code connections add   # configure a local server (base URL + model)
local-code                   # launch the TUI
```

See `TODO.md` for known v1 limitations.
