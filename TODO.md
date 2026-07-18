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

8. **Workspace panes have one split axis per window and no resizing.** A window's first split
   (`C-b %` or `C-b "`) fixes its layout axis; later splits extend along that axis (the other
   direction's chord is honored but its direction is ignored), and all panes are equal-sized.
   Mixed-direction nesting is blocked by ntui's sibling-scoped keyed reconciliation — a nested
   split tree would reparent (and thus unmount/reset) live sessions on every split. Revisit if
   ntui grows global keys/portals. Workspace layout also isn't persisted across restarts — each
   pane's *session* is individually resumable, but the window/pane arrangement resets.

9. **The two live smoke tests (`live_compact`, `live_init`) only assert non-empty output.**
   Neither checks structural correctness of the generated content (e.g., that `/init`'s output
   looks like real markdown, or that `/compact`'s summary is actually shorter than the input).
   They'd miss a regression where the model returns garbage-but-nonempty text. Intentionally thin
   smoke-test bar, consistent with the pre-existing Phase 2 live tests.

