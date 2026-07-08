# Known v1 Limitations

These are documented, accepted trade-offs surfaced during Phase 4 (slash commands & session
persistence) code review — not bugs, but gaps worth revisiting post-v1.

1. **No cancel/escape for pending numbered-choice menus.** `/model`, `/permissions`, `/resume`,
   and the permission-request prompt all intercept digit keys while a choice is pending. Any
   non-matching keystroke is silently swallowed until a valid digit is pressed — there's no way
   to back out once one of these menus is open.

2. **MCP tool discovery only happens once, at TUI startup.** `/model` and `/resume` rebuild the
   agent but reuse the already-discovered `NamespacedMcpTool`s rather than reconnecting to MCP
   servers. A server added to `mcp-servers.toml` mid-session isn't picked up until a full TUI
   restart. Deliberate trade-off, not an oversight.

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

7. **Headless mode doesn't load AGENTS.md/CLAUDE.md context.** This gap was already open at the
   end of Phase 2 and remains open — it was out of scope for this TUI-focused plan.

8. **HTTP/WebSocket MCP transports are only proven via negative/graceful-degradation tests.**
   Only the stdio transport has a live, fixture-server integration test proving positive
   end-to-end behavior. Inherited from Phase 5, not introduced here.

9. **The two live smoke tests (`live_compact`, `live_init`) only assert non-empty output.**
   Neither checks structural correctness of the generated content (e.g., that `/init`'s output
   looks like real markdown, or that `/compact`'s summary is actually shorter than the input).
   They'd miss a regression where the model returns garbage-but-nonempty text. Intentionally thin
   smoke-test bar, consistent with the pre-existing Phase 2 live tests.

10. **Headless mode registers the `skill` tool but doesn't auto-inject skill context.** Like
    limitation #7 (no AGENTS.md/CLAUDE.md context in headless), `-p` prompts get the `skill` tool
    for model-invoked skills but never see the auto-injected bodies of `alwaysApply`/glob-matched
    `.mdc` skills, since headless mode doesn't thread `extra_system_context` through at all.
    Deliberate scoping decision, not a bug — fixing it is the same follow-up as #7's.
