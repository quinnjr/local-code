# Known v1 Limitations

These are documented, accepted trade-offs surfaced during Phase 4 (slash commands & session
persistence) code review — not bugs, but gaps worth revisiting post-v1.

1. **No cancel/escape for pending numbered-choice menus.** `/model`, `/permissions`, `/resume`,
   and the permission-request prompt all intercept digit keys while a choice is pending. Any
   non-matching keystroke is silently swallowed until a valid digit is pressed — there's no way
   to back out once one of these menus is open.

2. **MCP tool discovery only happens once, at TUI startup — except via `/mcp add`.** `/model` and
   `/resume` rebuild the agent but reuse the already-discovered `NamespacedMcpTool`s rather than
   reconnecting to MCP servers. A server added by hand-editing `mcp.toml` mid-session isn't picked
   up until a full TUI restart; `/mcp add` is the one path that *does* connect and merge its tools
   into the live agent immediately. Deliberate trade-off for the hand-edit case, not an oversight.

3. **`/clear` doesn't rebuild the live agent object.** It resets the visible transcript and
   session file immediately, but the agent's in-memory `SeededMemory` technically retains
   pre-clear history until the next `/compact` or agent rebuild.

4. **`/compact`'s truncation boundaries aren't perfectly aligned.** Message-level (`Vec<Message>`)
   and transcript-level (`Vec<TranscriptEntry>`) truncation are computed independently by count,
   and one turn can span several transcript entries. Documented approximation, not exact.

5. **`SlashContext` carries a separate `model: SharedModel` field alongside `agent: Arc<Agent>`.**
   The vendored `daimon` crate has no public accessor for an `Agent`'s model, so the two can't be
   derived from each other — they're kept in lockstep manually at every rebuild site instead.

6. **`/init`'s test mutates the process-wide current directory.** Flagged as a minor test-hygiene
   wart (run single-threaded if flakiness appears) — not a product-behavior limitation.

7. **HTTP/WebSocket MCP transports are only proven via negative/graceful-degradation tests.**
   Only the stdio transport has a live, fixture-server integration test proving positive
   end-to-end behavior. Inherited from Phase 5, not introduced here.

8. **`list_sessions` fully parses every session file, including eagerly-created empty ones.**
   Every launch and every new tab/pane writes its (empty) session file up front — deliberate,
   because the timestamped path allocation doubles as filename reservation (two panes created in
   the same second would otherwise collide) and because pane creation surfaces disk errors
   immediately. The cost is that `/resume` reads and JSON-parses each empty file just to skip it.
   A byte-length pre-filter was rejected as fragile (empty-file size varies with connection/model
   name lengths). The write side of the same loan: pane creation performs that small session-file
   write synchronously inside the workspace's key handler (sub-millisecond on a local FS; would
   stall the render thread on a slow/networked mount). Revisit only if a resume listing ever gets
   slow or pane creation is ever observed to hitch.

9. **Workspace panes have one split axis per window and no resizing.** A window's first split
   (`C-b %` or `C-b "`) fixes its layout axis; later splits extend along that axis (the other
   direction's chord is honored but its direction is ignored), and all panes are equal-sized.
   Mixed-direction nesting is blocked by ntui's sibling-scoped keyed reconciliation — a nested
   split tree would reparent (and thus unmount/reset) live sessions on every split. Revisit if
   ntui grows global keys/portals. Workspace layout also isn't persisted across restarts — each
   pane's *session* is individually resumable, but the window/pane arrangement resets.

10. **The two live smoke tests (`live_compact`, `live_init`) only assert non-empty output.**
   Neither checks structural correctness of the generated content (e.g., that `/init`'s output
   looks like real markdown, or that `/compact`'s summary is actually shorter than the input).
   They'd miss a regression where the model returns garbage-but-nonempty text. Intentionally thin
   smoke-test bar, consistent with the pre-existing Phase 2 live tests.


11. **MCP connections added at runtime via `/mcp add` are not closed at exit.** `run_tui` retains
   the startup tool set and closes those server connections in an orderly way after the render
   loop exits (`mcp::connect::close_all`, added with daimon 0.22.1's `McpToolBridge::close`), but
    tools discovered by an in-TUI `/mcp add` live in per-pane `mcp_tools_state` inside `App`
    components, which `run_tui` cannot reach after `ntui::render` returns — those children are
    reaped by process teardown breaking their pipes, exactly as all connections were before.
    Revisit if a shared connection registry ever gets threaded through `AppProps`.

12. **The `serve_artifacts` HTTP server has no shutdown and is of limited use headless.** Once
   started it serves until process exit — deliberate, so URLs survive agent rebuilds and every
   pane's tool instance shares the process-wide registry. In headless mode (`-p`) the returned
   URL is only reachable while that turn is still running; the TUI is the real showcase
   surface. Revisit if artifacts ever need to outlive the process.

13. **`serve_artifacts`'s execute-level tests run the real tool against the process CWD.** Same
   test-hygiene wart class as #6, one notch sharper:
   `artifacts::tool::tests::execute_runs_against_the_process_cwd` and
   `agent::build::tests::built_agent_can_call_serve_artifacts` run with no tempdir, so `cargo
   test` creates and binds a live server to the crate root's real (gitignored)
   `.local-code/artifacts/` — the same directory a real session in this checkout serves. Keep
   the suite free of `set_current_dir` mutators or these tests flake or vacuously pass. Revisit
   if a project-root seam is ever threaded into the tool layer.

14. **The artifact server's path canonicalize+containment check and file open are two syscalls.**
   Check-then-act: a symlink flipped between the check and the open could theoretically race the
   server into serving a file outside the artifacts root. Accepted for v1 — the only principals
   who can write the artifacts dir (the agent itself, same-user processes) could already copy
   files into it directly, so the race buys them nothing. Revisit if the artifacts dir is ever
   writable by a less-trusted principal than the one reading via HTTP, or if
   `openat2(RESOLVE_BENEATH)`-style APIs become practical via the existing dep tree.

15. **`tui::app::tests::model_switch_updates_the_model_compact_uses` is flaky under CI load.**
   The paused-time (`start_paused`) test drives a real reqwest call to a dead port
   (`127.0.0.1:1`) behind a fixed 60×10ms tick budget; on a loaded runner the connection
   error can surface after the budget is exhausted, failing the assertion on the
   "compact failed" notice. Observed once on PR #15's CI; it passed on re-run and on every
   other run of the same tree (local runs, PR #14, the #14 merge commit's develop run).
   Revisit by waiting on the notice itself (poll the frame until it appears) instead of a
   fixed tick count if it flakes again.

16. **Plugin marketplaces install skills only, and catalogs are fetched live.** A Claude Code
    marketplace's plugins are installed by scanning their `skills/*/SKILL.md` dirs (or the catalog
    entry's `skills` paths); plugin `commands`/`agents`/`hooks`/`mcpServers` and `.claude-plugin/plugin.json`
    component config are ignored — LocalCode has no command/agent/hook concepts. `npm` plugin
    sources are unsupported, and `url`/`git-subdir` sources only accept https and SSH URLs on the
    three known skill hosts (no self-hosted). The marketplace registry is user-level only
    (`marketplaces.toml` in the user config dir), catalogs are re-fetched on every operation
    (no offline clone/cache like Claude Code's `~/.claude/plugins/`), and installed plugin skills
    are picked up only where skills are discovered — TUI startup and each headless run — so
    installing mid-session takes effect on the next TUI launch, not via `/model` rebuilds.

17. **Pasting into the input box or a wizard works only for single-line content, and pasted
    newlines act as Enter.** ntui 0.2.0 doesn't enable bracketed paste, so a paste arrives as
    ordinary key events: chars insert fine, but every pasted newline submits the current
    input/wizard step (a trailing newline usually does the right thing by accident; embedded
    newlines cascade answers). Atomic, newline-safe paste lands with ntui's
    `feat/bracketed-paste` branch (`use_paste` hook, `TestTerminal::send_paste`): once a
    release with it is published and the dependency bumped, wire `use_paste` into `App`'s
    input handler to push pasted text into `input_buffer` whole — sanitizing newlines to
    spaces while a wizard is active (single-line fields), inserting verbatim in the normal
    input box. The same ntui branch adds programmatic copy (`AppHandle::copy_to_clipboard`,
    OSC 52): after the bump, also wire a keybinding (e.g. Ctrl+Y) that yanks the last
    assistant message to the system clipboard. (Mouse-selection copy already works today —
    ntui never captures the mouse.)
