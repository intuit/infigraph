# DESIGN — Data-driven integration artifacts (upstream)

**Status:** Approved (design phase) — 2026-08-09
**Scope:** `crates/infigraph-cli/src/config_targets.rs`, `crates/infigraph-cli/src/hooks.rs`, `crates/infigraph-cli/src/install.rs`. Destined for **upstream** (`intuit/infigraph`), not the fork — requires explicit user approval before pushing/opening the PR.

## Motivation

Two related upstream issues:

- **intuit/infigraph#29** — `infigraph install` writes incompatible MCP configurations for several agents. `config_targets.rs`'s `AGENT_TARGETS` hardcodes a single `mcpServers`-shaped JSON writer (`install_json_target`) shared by every JSON-based agent, and a single TOML writer (`install_toml_target`) for Codex. Several agents need a genuinely different file path and/or JSON shape, not just a different top-level key.
- **intuit/infigraph#50** — "Default installation is too invasive." Wants install *modes* (mcp-server-only / automated / full hooks+session-recording), user-selectable at install time. Out of scope to fully implement here; this design lays groundwork by making `cmd_install()`'s monolithic call sequence composable.

This grew out of fork issue **pradeepmouli/infigraph#65** ("Agent/hook config is hardcoded in Rust source"), which proposed moving static config out of Rust source into bundled/data-driven files, mirroring the existing bundled → user → project discovery pattern already used for grammar plugins (`GRAMMAR_PLUGINS.md`).

During design, the scope widened past #65's original two halves (agent-target JSON config, hook scripts) to a third: `install.rs`'s `write_claude_md_instructions`/`write_editor_rules`/`write_reindex_command` hardcode markdown/rules content the exact same way. All three turned out to be one underlying problem — bundled content, written to a per-target path, merged into whatever's already there — so this design unifies them into one mechanism instead of three parallel ones.

### Precedent, and why it's not a straight copy

Two existing plugin systems already solve "extend without a recompile":

- **`infigraph-pipeline-plugin`** (`PIPELINE_PLUGINS.md`) — a `plugin.toml` manifest + a subprocess speaking newline-delimited JSON. Logic is fully externalized (any language, no rebuild ever) because the actual work — extracting structured metadata from arbitrary document text — is genuinely computational.
- **`infigraph-grammar-plugin`** (`GRAMMAR_PLUGINS.md`) — `.g4` grammar files + `plugin.toml` are hot-loaded data, but the actual extraction *logic* is a Java class compiled into the driver JAR, loaded by class name via reflection inside one shared JVM process. Adding new logic requires a JAR rebuild. Only the grammar/config is externalized, not the logic.

Neither is copied wholesale. What this design needs is closer to pipeline-plugin's shape (subprocess escape hatch for real logic) but for a task — "write a known, small fragment into a known file, preserving everything else" — that almost never needs a subprocess at all. So the default path stays pure data (no process spawn), with a subprocess **resolver** as an explicit, narrow escape hatch, not the default mechanism. Rust `dlopen`/`libloading`-based dynamic plugins were considered and rejected: no stable Rust ABI across compiler/dependency versions (real risk of UB), no crash isolation (a bad plugin can corrupt the host process, which matters when this code path writes into a user's real dotfiles during `infigraph install`), and it would be a third extensibility mechanism in this codebase rather than reusing the one (subprocess+JSON) already proven by pipeline-plugin.

### Invertibility constraint

A deliberate design goal, not yet built in this PR: it should eventually be possible to capture a live, hand-customized, or project-reference configuration and turn it into a new default template for that user (this becomes the new user-level override — see "Out of scope" for why the actual capture command is deferred). That's only tractable if the install operation is a deterministic function of `(bundled/user content, params) → target file`. A resolver-generated value is opaque — there's no way to look at a live config it produced and reconstruct the template that made it. So bundled/static content is the default and the only thing that's ever capturable; the resolver escape hatch is explicitly excluded from capture, by construction, whenever it's used.

## Research: verified per-agent MCP schemas

Issue #29's reporter manually tested GitHub Copilot CLI and OpenCode; VS Code, Zed, Windsurf, and Kiro were doc-comparison only, not tested. This design independently re-verified all four against current docs (2026-08-09) before committing to shapes:

| Agent | Path | Key path | Entry shape | Verification |
|---|---|---|---|---|
| Claude Code | `~/.claude.json` | `mcpServers.infigraph` | `{command, args}` | unchanged (existing `CLAUDE_CODE_SPECIAL` behavior) |
| Cursor, Gemini CLI, Aider | unchanged | `mcpServers.infigraph` | `{command, args}` | unchanged — #29 confirms these already work |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | `mcpServers.infigraph` | `{command, args}` | verified via docs.devin.ai/desktop/cascade/mcp (docs.windsurf.com redirects here) |
| Kiro | `~/.kiro/settings/mcp.json` (was `~/.kiro/mcp.json`) | `mcpServers.infigraph` | `{command, args}` | verified via kiro.dev docs — shape unchanged, only path was wrong |
| GitHub Copilot CLI | `~/.copilot/mcp-config.json` (was `mcp.json`) | `mcpServers.infigraph` | `{command, args}` | per #29 (reporter manually tested) |
| OpenCode | `~/.config/opencode/opencode.json` (was `~/.opencode/config.json`) | `mcp.infigraph` | `{type: "local", command: [mcp_path, "--mcp"]}` | per #29 (reporter manually tested) |
| VS Code | OS-default profile path (resolver — see below), was `~/.vscode/mcp.json` | `servers.infigraph` | `{command, args}` | verified via code.visualstudio.com/docs/agent-customization/mcp-servers. Real user-level path is profile-directory-based, not a fixed dotfile (undocumented on that page) |
| Zed | Zed's own `settings.json` (was separate `~/.zed/mcp.json`, which is silently ignored) | `context_servers.infigraph` | `{command, args, env: {}}` | verified via zed.dev/docs/ai/mcp — embeds into general settings, high JSONC risk (see below) |
| Codex | unchanged | (raw TOML section splice) | unchanged | unchanged — existing `install_toml_target` logic reused as the `toml_section` strategy |

## Architecture

### The core idea: most artifacts need no manifest entry at all

A file's **extension** plus its **mirrored path** are enough to know what to do with it — registering it explicitly in a manifest is only needed for the genuine exceptions. This removes the per-hook, per-agent bookkeeping (`key_path`, `idempotency_key`, `self_heal_field`, ...) almost entirely:

- **`.json` or `.toml` file at a path mirroring a real config file** → **deep-merge**. Object keys merge recursively (anything the fragment doesn't mention is preserved untouched). Array entries are trickier — a naive value-tree merge would either duplicate entries on every install or nuke a user's unrelated entries — so array merging uses one small, universal rule instead of per-artifact metadata: **any existing array entry whose serialized content already contains the substring `infigraph` is ours; remove it and replace it with the fragment's corresponding entries, leaving every other entry in that array untouched.** Since every infigraph script/command path already contains `infigraph-` by naming convention, this needs no separate declaration, and it makes self-healing automatic for free — whatever's in the fragment right now becomes the truth on every apply, no drift-tracking needed.
- **Everything else at a mirrored path** (`.sh`, `.md`, `.mdc`) → **overwrite**, byte for byte.

Both apply purely from a bundled file's *existence*, co-located at its own mirrored path within the integration's directory. That's the actual dividing line — not "which strategy does this need," but **is the content physically where the destination's mirrored path says it should be.** A manifest entry is needed whenever it isn't, for either of two reasons:

1. **The content lives elsewhere** (`shared/`) — CLAUDE.md's instructional text is `../shared/agents.md`, not a local file in `claude-code/`, so discovery can't find it by mirrored-path convention alone. This happens to also need `marker_delimited` (CLAUDE.md already has other content worth preserving), but the *reason* it's registered at all is the shared path, not the strategy — a plain `overwrite` artifact pulling from `shared/` would need the exact same registration. Cursor's and Windsurf's rules content is the same shared text (no local copy, no per-agent duplication) — so they need a minimal `config.toml` too, purely to say "pull this from `../shared/agents.md`," with `overwrite` as the strategy, not `marker_delimited`.
2. **There's no fixed local path to mirror at all** (VS Code, Zed) — a resolver computes it.

This has a forward-looking implication worth naming: today every hook script lives locally, one per integration, nothing shared at that level (the reindex skill is the one exception — see "Reindex as a shared skill" below) — but if a future hook ever became genuinely identical across integrations, it would need the same treatment as CLAUDE.md/Cursor/Windsurf/the reindex skill: an `[[artifact]]` entry pointing `content_file` at `../shared/hooks/...`, even with a plain `overwrite` strategy and nothing to "transform." The manifest requirement tracks *where the content lives*, never what happens to it once found.

Gemini CLI, OpenCode, Aider, Kiro, and GitHub Copilot CLI need **no `config.toml` at all** — just a bundled fragment file, locally, at the mirrored path. Claude Code, Cursor, and Windsurf need one because their instructional content is shared. VS Code and Zed need one because their path is resolver-computed. Codex needs one for the same shared-content reason as Claude Code — not for its MCP registration (still convention-based, local), but for the shared reindex skill entry (see "Reindex as a shared skill" below).

### Directory layout

One directory per integration (matching `infigraph-pipeline-plugin`'s own `plugin.toml`-alongside-content precedent), content mirroring the *real* destination folder structure below the agent's own root — not an invented organizational scheme:

```
crates/infigraph-cli/resources/integrations/     (bundled, compiled in)
~/.infigraph/integrations/                        (user-level override, identical structure)

  shared/                        # content referenced by more than one integration -- not duplicated per-agent
    agents.md                    # the core instructional text (today's write_claude_md_instructions block)
    skills/
      infigraph-reindex/
        SKILL.md                 # the reindex command, as an Agent Skills-format skill -- see "Reindex as a shared skill" below

  claude-code/
    config.toml                 # CLAUDE.md's marker-delimited entry, plus the shared reindex skill (both pull from shared/)
    .claude.json                # convention: JSON deep-merge, no manifest entry -- {"mcpServers":{"infigraph":{...}}}
    settings.json                # convention: JSON deep-merge -- literally the {"hooks": {...}} structure, in full
    hooks/
      enforce.sh                 # convention: overwrite (mirrors ~/.claude/hooks/) -- referenced BY PATH from settings.json
      edit-tracker.sh
      session-save.sh
      session-reset.sh
      session-start.sh
      session-end-save.sh
      clear-suggest.sh
      clear-guard.sh
      test-context-sentinel.sh
      search-fallback-sentinel.sh

  cursor/
    config.toml                   # rules content is shared -- see "Shared content" below
    .cursor/mcp.json              # convention: JSON deep-merge, no manifest entry needed for this one
    rules/                        # empty locally -- content_file in config.toml points at ../shared/agents.md

  windsurf/
    config.toml                   # same reason as Cursor
    .codeium/windsurf/mcp_config.json
    rules/

  vscode/       config.toml       # resolver-based path -- the one thing that can't be a mirrored file
  zed/          config.toml       # resolver-based path
  codex/
    config.toml                   # NEW: needed now only for the shared reindex skill entry (see naming-collision note below)
    .codex/config.toml            # convention: TOML section splice, no manifest entry -- unrelated to the manifest above despite the shared filename
  gemini-cli/       .gemini/settings.json
  opencode/         .config/opencode/opencode.json
  aider/            .aider/mcp.json
  kiro/             .kiro/settings/mcp.json
  github-copilot-cli/  .copilot/mcp-config.json
```

Note the naming collision this makes visible: `codex/.codex/config.toml` is a *content file* (the destination fragment, mirroring Codex's real `~/.codex/config.toml`), not to be confused with the *manifest* file `codex/config.toml` at the integration directory's own root (needed now only for the shared reindex skill entry — Codex's MCP registration itself still needs no manifest, since that content stays local and convention-based). Worth flagging explicitly during implementation naming/discovery logic (e.g. by requiring the manifest to live at the integration directory's *own root*, `<name>/config.toml`, never nested under a subdirectory) so the two are never ambiguous.

### Shared content

Content that's identical across multiple integrations lives once, under `shared/`, referenced via a relative path that escapes the integration's own directory (`../shared/agents.md`). Every integration pulling from `shared/` needs a `config.toml` entry for it — the strategy varies (CLAUDE.md needs `marker_delimited` since it may already hold other content worth preserving; Cursor and Windsurf's rules files are infigraph-owned outright, so plain `overwrite` is enough), but the registration itself is required in both cases, for the same reason: the content isn't where the mirrored path would suggest.

```toml
# claude-code/config.toml
label = "Claude Code"

[[artifact]]
path = ".claude/CLAUDE.md"
strategy = "marker_delimited"
start = "<!-- infigraph-primary-search -->"
end = "<!-- /infigraph-primary-search -->"
content_file = "../shared/agents.md"
```

```toml
# cursor/config.toml
label = "Cursor"

[[artifact]]
path = ".cursor/rules/infigraph.mdc"
strategy = "overwrite"
content_file = "../shared/agents.md"
```

If Cursor's `.mdc` format ends up needing its own frontmatter wrapper around the shared body (rather than the shared text working verbatim), that's a small addition to the artifact's fields at plan time (a `prefix`/`suffix` around `content_file`, or a per-format wrapper template) — not a change to *whether* this needs registration, only to what the registered artifact looks like.

`shared/` is discovered and overridable the same way as everything else — `~/.infigraph/integrations/shared/agents.md` overrides the bundled copy for every integration that references it, in one edit.

### Reindex as a shared skill

`/infigraph-reindex` is not Claude-Code-specific content — it's a generic "run `infigraph index`" instruction that happens to have been written as a Claude Code slash command purely because slash commands were the only cross-tool-adjacent format available when it was first added. Agent Skills (`agentskills.io`) is the right format for it now: a real, Linux Foundation–governed open standard, adopted by 32+ tools as of 2026 (Claude Code, Codex, Cursor, Gemini CLI, Windsurf, Zed, GitHub Copilot, AWS Kiro, and more), with a `SKILL.md` file (YAML frontmatter — only `name` and `description` required — plus the instructional body) as its unit of content. So this PR converts it: `shared/skills/infigraph-reindex/SKILL.md` replaces the old `claude-code/commands/infigraph-reindex.md`, registered the same way `shared/agents.md` is — via a `config.toml` `[[artifact]]` entry, since the content lives in `shared/`, not locally.

The Skills *format* is standardized; each tool's skill *discovery path* is not — every tool follows its own `.{name}/skills/` convention, so each integration needs its own verified path, the same diligence already applied to the MCP-path research table above. Two are verified for this PR:

```toml
# claude-code/config.toml (in addition to the CLAUDE.md artifact above)
[[artifact]]
path = ".claude/skills/infigraph-reindex/SKILL.md"
strategy = "overwrite"
content_file = "../shared/skills/infigraph-reindex/SKILL.md"
```

```toml
# codex/config.toml (Codex's first config.toml manifest — its MCP registration stays local/convention-based)
label = "Codex"

[[artifact]]
path = ".codex/skills/infigraph-reindex/SKILL.md"
strategy = "overwrite"
content_file = "../shared/skills/infigraph-reindex/SKILL.md"
```

The remaining skills-adopting integrations in this design (Cursor, Gemini CLI, Windsurf, Zed, GitHub Copilot CLI, Kiro) each get the equivalent entry, but their exact discovery paths are **not asserted here** — they need the same per-tool doc verification as VS Code/Zed's MCP paths before being written into a real `config.toml`, deferred to implementation time rather than guessed now. OpenCode and Aider are not confirmed skills-adopters as of this design and are left out of the skill rollout (they keep MCP-only `config.toml`-free registration).

This PR intentionally does **not** also move CLAUDE.md's core instructional content (`shared/agents.md`) into a shared skill — that's a larger, separately-scoped change (marker-delimited insertion into an existing user file vs. a skill's own dedicated file, different discovery semantics, different risk profile) deferred to its own follow-up.

### Path resolution and the resolver escape hatch

The destination root is `$HOME` by default. Two cases don't fit a mirrored path at all, and need `config.toml` for a `resolver` field instead:

- **VS Code** — the real user-level config lives in a profile directory that varies by OS and isn't fully knowable from a fixed dotfile.
- **Zed** — `context_servers` isn't a separate file at all; it's a section inside Zed's own `settings.json`, whose canonical path also varies by OS.

```toml
# zed/config.toml
[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]   # any executable; stdin/stdout JSON IPC, same shape pipeline-plugin uses
```

Resolver contract — one shape reused everywhere a resolver is allowed (path resolution; content generation):

- **stdin:** `{"mcp_path": "...", "os": "macos", "home": "/Users/..."}`
- **stdout:** `{"status": "ok", "data": {"path": "...", "content": ...}}` (the shape of `content` matches whatever the strategy expects) / `{"status": "skip", "message": "..."}` / `{"status": "error", "message": "..."}`

A resolver may also *generate content* instead of (or in addition to) resolving a path — e.g. a future hook whose script body needs to differ by detected environment. Per the invertibility constraint above, any artifact using a resolver is excluded from future capture/promote tooling — this is the cost of the escape hatch, and it's why it stays opt-in per-artifact rather than the default.

### JSON parse safety

Any JSON deep-merge target is parsed with `serde_json::from_str` before patching. **On parse failure** (comments, trailing commas — expected for Zed's general settings file, possible for others a user hand-edited): do not write. Report `Skipped { reason, manual_snippet }` with the exact fragment to add by hand, and move on — never risk corrupting a file that can't be safely round-tripped. No JSONC-tolerant parser is introduced; this is a deliberate scope boundary.

### Discovery

Two-tier — **not** the full three-tier grammar-plugin pattern, since which agent CLIs/tools are installed is a per-machine fact, not a per-project one:

1. **Bundled defaults** — the `crates/infigraph-cli/resources/integrations/` tree, compiled in. A `build.rs` walks the tree at compile time and generates the `&[(&str, &str)]` registry (relative path → contents) automatically — every bundled file, manifest or not, gets the same "drop a file in, done" ergonomics.
2. **User-level override** — `~/.infigraph/integrations/`, identical structure. A file at the same relative path overrides its bundled counterpart entirely; a new file adds a new convention-based artifact with zero Rust changes; a whole new `<name>/` directory adds a new integration.

### `CLAUDE_CODE_SPECIAL` removal

Claude Code's existing bespoke-path special case (`config_file == "CLAUDE_CODE_SPECIAL"` string match in `cmd_install`) becomes an ordinary convention-based artifact: `claude-code/.claude.json` deep-merges into `~/.claude.json`, no manifest entry, no magic string.

### Uninstall symmetry

Each mechanism defines its own inverse:

- **Convention-based JSON/TOML deep-merge**: the bundled fragment's own key structure *is* the removal instruction — delete exactly the keys/sections the fragment declares (e.g. `claude-code/.claude.json`'s `{"mcpServers": {"infigraph": {...}}}` tells uninstall to remove `mcpServers.infigraph`; no separate `key_path` field needed for either direction, since the fragment's shape already carries it). Array entries: remove any entry matching the `infigraph` ownership marker.
- **Convention-based overwrite**: delete the file.
- **`marker_delimited`**: remove the text between the markers declared in `config.toml`.

This replaces today's hardcoded `mcpServers`/`[mcp_servers.infigraph]` assumptions in `uninstall_json_target`/`uninstall_toml_target` — uninstalling OpenCode now correctly clears `mcp.infigraph`, derived from its own bundled fragment's shape, not a hardcoded key.

## `cmd_install` refactor (#50 groundwork)

Every artifact resolves to a coarse `InstallStep` (explicitly for `[[artifact]]` entries, inferred by location for convention-based ones — see below) so groups can be applied independently — no new CLI flag in this PR, but the surface exists for #50's future `--mode` flag:

```rust
pub(crate) enum InstallStep {
    McpRegistration,   // every mcp.json-shaped artifact across all integrations
    DocsAndRules,      // CLAUDE.md, editor rules, reindex command
    Hooks,             // hook scripts + their settings.json wiring
    Models,            // unchanged, not artifact-based
}
impl InstallStep {
    const ALL: &'static [InstallStep] = &[
        InstallStep::McpRegistration,
        InstallStep::DocsAndRules,
        InstallStep::Hooks,
        InstallStep::Models,
    ];
}

pub(crate) fn cmd_install(steps: &[InstallStep]) -> Result<()> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let mcp_path = find_mcp_binary()?;

    for step in steps {
        match step {
            InstallStep::McpRegistration | InstallStep::DocsAndRules | InstallStep::Hooks => {
                apply_artifacts_for_step(*step, &home, &mcp_path)?;
            }
            InstallStep::Models => install_models(&mcp_path, &home)?,
        }
    }
    crate::hooks::install_claude_allowlist(&home)?; // stays outside the artifact mechanism -- see below
    Ok(())
}
```

`apply_artifacts_for_step` discovers every artifact for that step — both the explicit `[[artifact]]` entries in whatever `config.toml`s exist, and every convention-based file not claimed by one. Since neither kind has a dedicated field to declare a step in, classification falls back to a simple, fixed rule based on the artifact's *destination* path (its `path` field for explicit entries, its mirrored path for convention-based ones): anything under `hooks/` is `Hooks`; anything under `rules/` or `skills/`, or a `marker_delimited` entry, is `DocsAndRules` (this covers CLAUDE.md, Cursor/Windsurf's rules, and every integration's reindex skill entry); everything else (the top-level MCP-config fragment, wherever it's convention-based or resolver-computed) is `McpRegistration`. This heuristic only needs to be *good enough*, not exact — no `--mode` flag ships in this PR to expose the distinction yet; it's groundwork for #50, not #50 itself.

`install_claude_allowlist` stays a plain, untouched function — it writes to `settings.local.json`'s `permissions.allow` list, which is a grant list, not "content deployed to a target path," so it doesn't fit the artifact shape.

## Deliberate behavior changes (call out explicitly in the PR description)

- **Matcher self-healing becomes universal and automatic.** Today, only `install_enforcement_hook` updates an existing entry's `matcher` in place if it's drifted from the current hardcoded value; every other hook only checks presence/absence. The convention-based engine gets this for free for every hook — since the whole `hooks.<Event>` array's infigraph-owned entries are fully replaced (remove-by-ownership-marker, then re-append from the fragment) on every apply, whatever's in `settings.json` right now is always correct, no separate drift-tracking needed. Strictly additive (closes a real gap: e.g. if `session-reset`'s matcher ever needs to change in a future release, existing installs currently would *not* pick that up automatically).
- **`install_edit_tracker_hook`'s "merge into an existing matcher-containing entry" special case is dropped.** It's the one function that, instead of always creating its own entry, searches for *any* existing `PostToolUse` entry whose matcher contains `"Edit"` and appends into that entry's `hooks` array — which could mutate an entry that isn't infigraph's. The convention-based engine always owns exactly the entries it recognizes via the `infigraph` marker, never touching a foreign entry regardless of matcher overlap. Net effect on an existing install: one extra `PostToolUse` array entry with the same `Edit|Write|NotebookEdit` matcher string as before — functionally identical (Claude Code doesn't care about duplicate matcher strings across entries), just not deduplicated at the JSON level anymore.

## Testing

- **Convention-engine unit tests**: JSON deep-merge (create-from-empty, preserve unrelated keys at any depth, nested-key creation), array-entry ownership (replace entries containing `infigraph`, leave everything else in the array untouched, handle an array that doesn't exist yet), TOML section splice (preserve unrelated sections/comments via raw text, not full round-trip), overwrite, JSON-parse-failure → `Skipped` (not a write).
- **Per-artifact fixture tests**: for every bundled fragment/script (all integrations), apply against an empty target and against a target with pre-existing unrelated content (including a target that already has *other tools'* array entries mixed in, for the hooks case), assert exact expected output. This is what actually catches a future fragment typo, not just the generic engine.
- **Discovery override test**: a file at the same relative path under `~/.infigraph/integrations/` is used instead of its bundled counterpart; a wholly new file (convention-based, no manifest) is discovered and applied; a wholly new integration directory is discovered and applied.
- **Uninstall test per mechanism**: at least one convention-based JSON artifact, one convention-based array artifact, one TOML artifact, and the `marker_delimited` artifact, confirming uninstall derives what to remove from the fragment's own shape rather than a hardcoded key.
- **Settings.json multi-event test**: `claude-code/settings.json`'s fragment declares entries under multiple event keys in one file; applying it produces all of them, and reapplying doesn't duplicate any.
- **Matcher self-heal test**: apply, hand-edit the resulting matcher in the live `settings.json`, re-apply, assert it's restored to match the bundled fragment.
- **Resolver tests** (VS Code, Zed): a fake resolver script exercising `ok`/`skip`/`error` responses, confirming the engine handles all three without touching the target file on `skip`/`error`.
- **Shared-content test**: Claude Code's `content_file = "../shared/agents.md"` picks up the shared file; overriding `~/.infigraph/integrations/shared/agents.md` changes the applied output without `claude-code/config.toml` changing. Same test shape for `shared/skills/infigraph-reindex/SKILL.md` against both Claude Code's and Codex's `config.toml` entries — one shared override changes both integrations' applied output.
- **Manifest/content naming-collision test**: `codex/.codex/config.toml` (a content file that happens to be named `config.toml`, nested under a subdirectory) is correctly treated as convention-based content, not mistaken for a manifest — confirms discovery only treats `<name>/config.toml` (integration root, not nested) as the manifest, distinct from `codex/config.toml` (the real manifest, now present for the reindex-skill entry).

## Migration

Stale files at old wrong paths (e.g. `~/.opencode/config.json` from before this fix) are inert — the affected tools already don't read them (confirmed in #29's testing). This PR does not clean them up; worth a one-line callout in the PR description, not a feature.

## Out of scope

- **Capture/promote-to-default command** (e.g. reading a live or project-reference config and generating a new `~/.infigraph/integrations/...` override from it). This is the natural next step once the artifact primitive exists — the two-tier discovery already means such a generated file becomes the default for all future installs for that user, for free — but the capture/diff tooling itself is real, separately-scoped work, deferred to a follow-up PR.
- **Category-level shared sourcing for `hooks`/`commands`** (a `[commands]`/`[hooks]` manifest *section* — distinct from individual `[[artifact]]` entries — declaring `source = "../shared/commands"` plus an optional named `adapter` transform, so a whole category of files can come from a shared location and be format-converted per integration, rather than registering each shared file individually). Verified against the current (2026) ecosystem before deferring, not just "nothing needs it yet": **hooks lack an independent per-tool *execution* mechanism outside Claude Code today.** A `hooks:`/`triggers:` SKILL.md frontmatter convention exists as real prior art (e.g. the `skill-triggers` project), and Skills' own spec permits arbitrary optional frontmatter fields beyond the required `name`/`description` — but that convention compiles down into Claude-Code-style hook config rather than being natively executed by another runtime, so there's still no second execution target for a category-level `adapter` to convert toward. Slash commands have no cross-tool standard either. (Cross-tool *instructions* content is a different, already-real case — see the AGENTS.md note below — which is exactly why `shared/agents.md`, a single-file case, ships in this PR while the general category-level mechanism doesn't.) Revisit if a runtime ever ships its own native executor for SKILL.md hooks/triggers frontmatter, or a second agent ships a comparable commands mechanism.
- **Writing an actual project-root `AGENTS.md`.** AGENTS.md is a genuine, Linux Foundation–stewarded cross-tool standard as of 2026 (28+ tools including Cursor, Windsurf, Codex, Gemini CLI, Aider, Zed, Devin natively read it; 60,000+ repos use it) — real validation for `shared/agents.md`'s naming and concept. But it's explicitly *project-level* (build commands, test procedures, project-specific style rules, placed at a repo's root), while infigraph's actual content here is *user-level* (global "use infigraph MCP tools" instructions, written to `~/.claude/CLAUDE.md`, applying across every project) — a different concern despite the similar name. Whether `infigraph init` (a separate, project-scoped command) should also write a real project AGENTS.md is a legitimately interesting adjacent idea, but belongs in its own issue/design, not folded into this install-time PR.
- No `--mode` / install-tier flag or UX (that's #50 itself, a separate follow-up PR once this groundwork lands).
- No JSONC-tolerant parser (Zed's embedded-settings case bails with instructions instead).
- No project-level (three-tier) discovery for integrations.
- `install_claude_allowlist` is untouched (different settings file, different section — not artifact-shaped).
