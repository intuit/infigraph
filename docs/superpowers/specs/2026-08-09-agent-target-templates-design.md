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

### Directory layout — tree position *is* the destination path

```
crates/infigraph-cli/resources/integrations/     (bundled, compiled in)
~/.infigraph/integrations/                        (user-level override, identical structure)

  claude-code/
    .claude.json.toml              # patch descriptor -> ~/.claude.json
    .claude/
      CLAUDE.md.toml                # patch descriptor -> ~/.claude/CLAUDE.md
      CLAUDE.md.fragment.md         # the actual instructions block (referenced by the descriptor)
      commands/
        infigraph-reindex.md        # plain file -> deploys verbatim to ~/.claude/commands/infigraph-reindex.md
      hooks/
        infigraph-enforce.sh                # plain file -> ~/.claude/hooks/infigraph-enforce.sh
        infigraph-enforce.sh.toml           # patch descriptor: settings.json wiring for that script
        infigraph-edit-tracker.sh
        infigraph-edit-tracker.sh.toml
        infigraph-session-save.sh + .toml
        infigraph-session-reset.sh + .toml
        infigraph-session-start.sh + .toml
        infigraph-session-end-save.sh + .toml
        infigraph-clear-suggest.sh + .toml
        infigraph-clear-guard.sh + .toml
        infigraph-test-context-sentinel.sh + .toml
        infigraph-search-fallback-sentinel.sh + .toml

  cursor/
    .cursor/mcp.json.toml
    .cursor/rules/infigraph.mdc     # plain file

  vscode/       .vscode/mcp.json.toml   (resolver — see below)
  codex/        .codex/config.toml.toml # yes, config.toml.toml — the destination genuinely is config.toml
  gemini-cli/   .gemini/settings.json.toml
  zed/          settings.json.toml       (path itself needs a resolver — see below)
  opencode/     .config/opencode/opencode.json.toml
  aider/        .aider/mcp.json.toml
  windsurf/     .codeium/windsurf/mcp_config.json.toml
  kiro/         .kiro/settings/mcp.json.toml
  github-copilot-cli/  .copilot/mcp-config.json.toml
```

**Convention:**

- **A plain file** deploys verbatim to the destination given by its own path within the integration directory, relative to `$HOME` (or the resolved root). No manifest, no metadata — this is the `overwrite` strategy, and it's the default for anything that's a whole file with no merge concerns: hook scripts, the reindex command, single-purpose rule files.
- **A `<name>.toml` sidecar** means the *destination* is `<name>` (same path, minus the `.toml` suffix) and this file is a **patch descriptor**, not the full file — it declares a `strategy` and whatever that strategy needs, and either inline content or a `content_file` reference to a sibling file for anything large (markdown fragments, big JSON entries).

### Merge strategies

```rust
enum MergeStrategy {
    JsonKeyPath(Vec<String>),                      // set an exact value at a nested key, preserve siblings
    JsonArrayAppend {                                // hooks: find-or-append into an event array
        array_path: Vec<String>,                     // e.g. ["hooks", "PostToolUse"]
        idempotency_key: String,                      // substring match against existing hooks[].command
        self_heal_field: Option<String>,               // e.g. "matcher" -- update in place if drifted
    },
    TomlSection(String),                             // Codex: splice by [section header], preserve rest via raw text (not a full TOML round-trip, to preserve formatting/comments)
    MarkerDelimited { start: String, end: String },   // CLAUDE.md: replace text between two sentinel comments
    Overwrite,                                        // whole-file write; used implicitly by plain (non-.toml) files
}
```

Patch-descriptor sidecar shape (JSON strategy example, `.claude.json.toml`):

```toml
strategy = "json_key_path"
key_path = ["mcpServers", "infigraph"]

[entry]
command = "{{mcp_path}}"
args = ["--mcp"]
```

Hook wiring sidecar shape (`infigraph-enforce.sh.toml`):

```toml
strategy = "json_array_append"
array_path = ["hooks", "PreToolUse"]
idempotency_key = "infigraph-enforce"     # matched as a substring of existing hooks[].command values
self_heal_field = "matcher"
matcher = "Grep|Glob|Bash|Read|Write|Edit|Agent"
timeout = 5
```

`array_path` may be a single path (`Vec<String>`, e.g. `["hooks", "PostToolUse"]`) or multiple paths applied identically (`Vec<Vec<String>>`) — `install_session_end_hook`'s existing behavior (one script, registered on both `SessionEnd` and `PreCompact`) becomes one sidecar with `array_path = [["hooks", "SessionEnd"], ["hooks", "PreCompact"]]`, instead of two near-identical Rust code blocks. Both forms are TOML arrays, so the disambiguator isn't "is it an array" — parse as a `toml::Value` and check whether the *first element* is itself a `Value::Array` (multi-path) or a `Value::String` (single path); an empty array is invalid input, reject it explicitly rather than guessing a default. The same rule applies to `path`/`resolver`-adjacent fields elsewhere in this design that accept a bare value or a collection of them — always branch on the parsed `toml::Value` shape, never model as two separate typed fields (Serde's `#[serde(untagged)]` resolution order can silently pick the wrong variant on ambiguous input).

### Path resolution and the resolver escape hatch

The destination root is `$HOME` by default (a plain file/sidecar at `claude-code/.claude.json.toml` targets `~/.claude.json`). Two cases don't fit "tree position = destination" cleanly, both handled by a `resolver` field instead of a static path:

- **VS Code** — the real user-level config lives in a profile directory that varies by OS and isn't fully knowable from a fixed dotfile.
- **Zed** — `context_servers` isn't a separate file at all; it's a section inside Zed's own `settings.json`, whose canonical path also varies by OS.

For these, the sidecar has no fixed tree-relative destination; instead:

```toml
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]   # any executable; stdin/stdout JSON IPC, same shape pipeline-plugin uses
```

Resolver contract — one shape reused everywhere a resolver is allowed (path resolution here; content generation, described next):

- **stdin:** `{"mcp_path": "...", "os": "macos", "home": "/Users/..."}`
- **stdout:** `{"status": "ok", "data": {"path": "...", "content": ...}}` (the shape of `content` matches whatever the strategy expects) / `{"status": "skip", "message": "..."}` / `{"status": "error", "message": "..."}`

A resolver may also *generate content* instead of (or in addition to) resolving a path — e.g. a future hook whose script body needs to differ by detected environment. Same contract: the resolver's `data.content` becomes the artifact's content instead of a bundled file. Per the invertibility constraint above, any artifact using a resolver is excluded from future capture/promote tooling — this is the cost of the escape hatch, and it's why it stays opt-in per-artifact rather than the default.

### JSON parse safety

Any JSON-strategy artifact (`json_key_path`, `json_array_append`) parses the target file with `serde_json::from_str` before patching. **On parse failure** (comments, trailing commas — expected for Zed's general settings file, possible for others a user hand-edited): do not write. Report `Skipped { reason, manual_snippet }` with the exact fragment to add by hand, and move on — never risk corrupting a file that can't be safely round-tripped. No JSONC-tolerant parser is introduced; this is a deliberate scope boundary.

### Discovery

Two-tier — **not** the full three-tier grammar-plugin pattern, since which agent CLIs/tools are installed is a per-machine fact, not a per-project one:

1. **Bundled defaults** — the `crates/infigraph-cli/resources/integrations/` tree, compiled in. Rather than a hand-maintained list of `include_str!` calls that has to be edited every time an integration or hook is added, a `build.rs` walks the tree at compile time and generates the `&[(&str, &str)]` registry (relative path → contents) automatically — so the bundled tier gets the same "drop a file in, done" ergonomics the user-override tier already has by construction.
2. **User-level override** — `~/.infigraph/integrations/`, identical structure. A file at the same relative path overrides its bundled counterpart entirely (not merged field-by-field); a new file or whole new integration subdirectory adds to it with zero Rust changes or recompile.

### `CLAUDE_CODE_SPECIAL` removal

Claude Code's existing bespoke-path special case (`config_file == "CLAUDE_CODE_SPECIAL"` string match in `cmd_install`) becomes an ordinary artifact at `claude-code/.claude.json.toml` — same mechanism now covers every agent uniformly, no magic string.

### Uninstall symmetry

Each strategy defines its own inverse, read from the same descriptor used to install:

- `json_key_path` / `json_array_append`: remove the value at `key_path`/`array_path` (matched by `idempotency_key` for array entries), leaving everything else in the file untouched.
- `toml_section`: remove by section header (existing `uninstall_toml_target` logic, unchanged).
- `marker_delimited`: remove the text between the markers.
- `overwrite`: delete the file.

This replaces today's hardcoded `mcpServers`/`[mcp_servers.infigraph]` assumptions in `uninstall_json_target`/`uninstall_toml_target` — uninstalling OpenCode now correctly clears `mcp.infigraph`, not a nonexistent `mcpServers.infigraph`.

## `cmd_install` refactor (#50 groundwork)

Each artifact is tagged with a coarse `InstallStep` so groups can be applied independently — no new CLI flag in this PR, but the surface exists for #50's future `--mode` flag:

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

`apply_artifacts_for_step` discovers every artifact (bundled ∪ user-override) whose step tag matches, and calls `apply_artifact` on each. Existing call sites: `main.rs`'s `Commands::Install` dispatch passes `InstallStep::ALL`; `reinstall_hooks()` passes `&[InstallStep::Hooks]` (its existing behavior). Behavior is unchanged in this PR; only the internal surface now exists for a future subset.

`install_claude_allowlist` stays a plain, untouched function — it writes to `settings.local.json`'s `permissions.allow` list, which is a grant list, not "content deployed to a target path," so it doesn't fit the artifact shape.

## Deliberate behavior changes (call out explicitly in the PR description)

- **Matcher self-healing becomes uniform.** Today, only `install_enforcement_hook` updates an existing entry's `matcher` in place if it's drifted from the current hardcoded value; every other hook only checks presence/absence. The generic engine does this for every `json_array_append` artifact with a `self_heal_field` — strictly additive (closes a real gap: if e.g. `session-reset`'s matcher ever needs to change in a future release, existing installs currently would *not* pick that up automatically).
- **`install_edit_tracker_hook`'s "merge into an existing matcher-containing entry" special case is dropped.** It's the one function that, instead of always creating its own entry, searches for *any* existing `PostToolUse` entry whose matcher contains `"Edit"` and appends into that entry's `hooks` array — which could mutate an entry that isn't infigraph's. The generic engine always creates/owns its own entry, matching every other hook's existing behavior. Net effect on an existing install: one extra `PostToolUse` array entry with the same `Edit|Write|NotebookEdit` matcher string as before — functionally identical (Claude Code doesn't care about duplicate matcher strings across entries), just not deduplicated at the JSON level anymore.

## Testing

- **Strategy unit tests** (one set per `MergeStrategy` variant): create-from-empty, preserve-existing-unrelated-keys/sections/text, overwrite-existing-infigraph-entry, nested key creation, JSON-parse-failure → `Skipped` (not a write).
- **Per-artifact fixture tests**: for every bundled artifact (all integrations, all hooks, docs/rules), load it via the same `build.rs`-generated registry production code uses, apply against an empty target and against a target with pre-existing unrelated content, assert exact expected output. This is what actually catches a future template typo, not just the generic engine.
- **Discovery override test**: a file at the same relative path under `~/.infigraph/integrations/` is used instead of its bundled counterpart; a wholly new integration directory is discovered and applied.
- **Uninstall test per strategy**: at least one artifact per strategy, confirming uninstall reads the descriptor rather than a hardcoded key/section/marker.
- **Multi-target test**: the session-end hook's two-event `array_path` produces both entries from one apply call.
- **Matcher self-heal test**: apply, hand-edit the resulting matcher, re-apply, assert it's restored and reported as "updated".
- **Resolver tests** (VS Code, Zed): a fake resolver script exercising `ok`/`skip`/`error` responses, confirming the artifact engine handles all three without touching the target file on `skip`/`error`.

## Migration

Stale files at old wrong paths (e.g. `~/.opencode/config.json` from before this fix) are inert — the affected tools already don't read them (confirmed in #29's testing). This PR does not clean them up; worth a one-line callout in the PR description, not a feature.

## Out of scope

- **Capture/promote-to-default command** (e.g. reading a live or project-reference config and generating a new `~/.infigraph/integrations/...` override from it). This is the natural next step once the artifact primitive exists — the two-tier discovery already means such a generated file becomes the default for all future installs for that user, for free — but the capture/diff tooling itself is real, separately-scoped work, deferred to a follow-up PR.
- No `--mode` / install-tier flag or UX (that's #50 itself, a separate follow-up PR once this groundwork lands).
- No JSONC-tolerant parser (Zed's embedded-settings case bails with instructions instead).
- No project-level (three-tier) discovery for integrations.
- `install_claude_allowlist` is untouched (different settings file, different section — not artifact-shaped).
