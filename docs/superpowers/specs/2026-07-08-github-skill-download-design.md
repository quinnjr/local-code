# GitHub-powered skill downloading — design

## Purpose

Add a "skills" concept to local-code (mirroring Anthropic's Claude Code skills), installable
directly from GitHub repositories, with optional Cursor-style `.mdc` conditional auto-loading.

## Skill file formats

A skill is a directory containing exactly one of `SKILL.md` or `SKILL.mdc` (if both are present,
`.mdc` takes precedence — a malformed-repo edge case, not something to silently merge). Each file
has YAML frontmatter:

```yaml
---
name: pdf-extraction
description: Extract text and tables from PDF files
globs: ["*.pdf"]        # optional, .mdc only
alwaysApply: false      # optional, .mdc only, default false
---
(skill body / instructions, in Markdown)
```

Loading behavior by field combination:

- **Plain `SKILL.md`** (or `.mdc` with no `globs` and no `alwaysApply: true`): **model-invoked**.
  Listed by name + description for the model; body loaded on demand via the new `skill` tool.
- **`.mdc` with `alwaysApply: true`**: **always auto-injected** into the system prompt at agent
  build/rebuild time. Not separately model-invocable.
- **`.mdc` with `globs`** (and not `alwaysApply: true`): auto-injected at build/rebuild time iff
  the project's file tree contains at least one file matching any glob. Matching is done once,
  at the same point `context::load_project_context` runs (agent build/rebuild), using the same
  directory-walking/ignore rules as the existing `grep`/`glob` built-in tools (respects
  `.gitignore`). Not re-evaluated per turn — consistent with how CLAUDE.md/AGENTS.md context is
  already loaded once per build, not live per-turn.

## Module layout

New `src/skills/` module:

- `skills/types.rs` — `Skill` (name, description, scope, dir path, frontmatter fields),
  `SkillSource` (owner, repo, path, ref), `InstalledSkillManifest` (source + resolved commit SHA,
  serialized as JSON).
- `skills/github.rs` — parses `owner/repo[/path][@ref]` specs; resolves and recursively downloads
  a directory via the GitHub Contents API
  (`https://api.github.com/repos/{owner}/{repo}/contents/{path}?ref={ref}`, default ref = repo's
  default branch). Sends `Authorization: Bearer <token>` if a token is available via
  `SecretStore::get_api_key("github")`; otherwise unauthenticated (60 req/hr limit applies).
- `skills/install.rs` — install / update / remove / list operations against a scope's `skills/`
  directory.
- `skills/discovery.rs` — scans both scope directories, parses each skill's frontmatter, and
  produces: (a) the list of model-invoked skills (name + description) for context injection and
  the `skill` tool, and (b) the auto-injected bodies of matching/always-apply `.mdc` skills.

## Install location, scoping, and naming

- Project scope (default): `<project_config_dir>/skills/<name>/` (i.e. `.local-code/skills/<name>/`).
- Global scope (`--global`): `<user_config_dir>/skills/<name>/`.
- When a skill exists in both scopes under the same name, project scope shadows global for both
  discovery and the `skill` tool.
- `<name>` defaults to the last path segment of the source spec, or the repo name if no subpath
  was given; overridable with `--name`.
- Each install writes a sidecar `.skill-manifest.json` in the skill's directory recording
  `{owner, repo, path, ref, commit_sha}`, used by `update` to detect whether the pinned ref's
  head has moved.

## CLI surface

```
local-code skills install <owner>/<repo>[/<path>][@<ref>] [--global] [--name <name>]
local-code skills list                          # name, description, scope, source, format
local-code skills remove <name> [--global]
local-code skills update [<name>] [--global]    # re-resolves ref; re-fetches if commit SHA changed
                                                  # no name = update all installed skills in scope
```

Follows the existing `Connections`/`Memory` subcommand pattern in `src/cli/mod.rs`
(`ConnectionsAction`, `MemoryAction`) — a `SkillsAction` enum dispatched the same way.

## GitHub authentication

Optional. Reuses the existing keyring-backed `SecretStore` (`src/config/secrets.rs`) under a
fixed key, `"github"`, the same mechanism used for LLM connection API keys. No dedicated `skills
auth` subcommand in this pass — if a token is desired, it can be set via the existing secret-store
primitive; wiring a dedicated setter is out of scope unless later requested.

## Agent wiring

- New built-in `skill` tool in `agent/tools.rs`, registered in `agent/build.rs::register_all_tools`
  alongside the six existing built-ins, `GatedTool`-wrapped identically. Takes a skill `name`,
  returns its body content (frontmatter stripped). Only resolves model-invoked skills (plain
  `SKILL.md`, or `.mdc` without `globs`/`alwaysApply: true`); auto-injected `.mdc` skills are not
  separately invocable through this tool since their content is already in context.
- `context::load_project_context` (or a sibling function called alongside it, at the same
  agent-build/rebuild call sites) additionally:
  1. Enumerates installed skills across both scopes via `skills::discovery`.
  2. Appends the full body of every always-apply or glob-matched `.mdc` skill.
  3. Appends a short listing (name + description) of remaining model-invoked skills, directing
     the model to the `skill` tool to load them.

## Error handling

- Network/API errors (rate limit, 404, invalid spec) surface as clear `anyhow` error messages at
  the CLI layer — no silent partial installs. A failed install does not leave a partially-written
  skill directory (fetch fully into a temp location, then move into place atomically).
- Malformed frontmatter (missing `name`/`description`, invalid YAML) causes that skill to be
  skipped during discovery with a warning printed to stderr, rather than failing the whole agent
  build.

## Testing

- `skills::github` — unit tests for spec parsing (`owner/repo`, `owner/repo/path`, `owner/repo@ref`,
  `owner/repo/path@ref`); HTTP calls tested against a mock server (existing `mockito`/similar dev
  dependency if present, else a lightweight local HTTP fixture) rather than live GitHub, consistent
  with how `mcp_stdio_integration.rs` uses a fixture server instead of live services.
- `skills::discovery` — unit tests for frontmatter parsing, glob-matching against a `tempdir`-based
  fake project tree, and scope-shadowing (project overrides global by name).
- `skills::install` — unit tests for install/update/remove against `tempdir`s, including the
  manifest round-trip and atomic-install-on-failure behavior.
- `cli::skills` — unit tests mirroring `cli::connections`' generic-over-`Read`/`Write` style where
  interactive output is involved (e.g. `list`'s formatted output).
- `agent::tools` — unit test for the `skill` tool resolving a fixture skill directory and returning
  its body.
