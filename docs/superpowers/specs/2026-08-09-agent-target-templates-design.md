# DESIGN — Data-driven agent-target MCP config (upstream)

**Status:** Approved (design phase) — 2026-08-09
**Scope:** `crates/infigraph-cli/src/config_targets.rs`, `crates/infigraph-cli/src/hooks.rs`, `crates/infigraph-cli/src/install.rs`. Destined for **upstream** (`intuit/infigraph`), not the fork — requires explicit user approval before pushing/opening the PR.

## Motivation

Two related upstream issues:

- **intuit/infigraph#29** — `infigraph install` writes incompatible MCP configurations for several agents. `config_targets.rs`'s `AGENT_TARGETS` hardcodes a single `mcpServers`-shaped JSON writer (`install_json_target`) shared by every JSON-based agent, and a single TOML writer (`install_toml_target`) for Codex. Several agents need a genuinely different file path and/or JSON shape, not just a different top-level key.
- **intuit/infigraph#50** — "Default installation is too invasive." Wants install *modes* (mcp-server-only / automated / full hooks+session-recording), user-selectable at install time. Out of scope to fully implement here; this design lays groundwork by making `cmd_install()`'s monolithic call sequence composable.

This grew out of fork issue **pradeepmouli/infigraph#65** ("Agent/hook config is hardcoded in Rust source"), which proposed moving static config out of Rust source into bundled/data-driven files, mirroring the existing bundled → user → project discovery pattern already used for grammar plugins (`GRAMMAR_PLUGINS.md`). This PR addresses both halves of #65 in one submission: agent-target MCP config (`config_targets.rs`, driven by #29) and hook-script bundling (`hooks.rs`) — see the "Hooks bundling" section below.

## Research: verified per-agent schemas

Issue #29's reporter manually tested GitHub Copilot CLI and OpenCode; VS Code, Zed, Windsurf, and Kiro were doc-comparison only, not tested. This design independently re-verified all four against current docs (2026-08-09) before committing to shapes:

| Agent | Path | `key_path` | Entry shape | Verification |
|---|---|---|---|---|
| Claude Code | `~/.claude.json` | `mcpServers.infigraph` | `{command, args}` | unchanged (existing `CLAUDE_CODE_SPECIAL` behavior) |
| Cursor, Gemini CLI, Aider | unchanged | `mcpServers.infigraph` | `{command, args}` | unchanged — #29 confirms these already work |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` | `mcpServers.infigraph` | `{command, args}` | verified via docs.devin.ai/desktop/cascade/mcp (docs.windsurf.com redirects here) |
| Kiro | `~/.kiro/settings/mcp.json` (was `~/.kiro/mcp.json`) | `mcpServers.infigraph` | `{command, args}` | verified via kiro.dev docs — shape unchanged, only path was wrong |
| GitHub Copilot CLI | `~/.copilot/mcp-config.json` (was `mcp.json`) | `mcpServers.infigraph` | `{command, args}` | per #29 (reporter manually tested) |
| OpenCode | `~/.config/opencode/opencode.json` (was `~/.opencode/config.json`) | `mcp.infigraph` | `{type: "local", command: [mcp_path, "--mcp"]}` | per #29 (reporter manually tested) |
| VS Code | OS-default profile path (see below), was `~/.vscode/mcp.json` | `servers.infigraph` | `{command, args}` | verified via code.visualstudio.com/docs/agent-customization/mcp-servers. Real user-level path is profile-directory-based, not a fixed dotfile (undocumented on that page); using the well-known OS-default profile locations, Default profile only |
| Zed | Zed's own `settings.json` (was separate `~/.zed/mcp.json`, which is silently ignored) | `context_servers.infigraph` | `{command, args, env: {}}` | verified via zed.dev/docs/ai/mcp — embeds into general settings, high JSONC risk (see below) |
| Codex | unchanged | (raw TOML section splice) | unchanged | unchanged — existing `install_toml_target` untouched |

VS Code path table: `macos = "~/Library/Application Support/Code/User/mcp.json"`, `linux = "~/.config/Code/User/mcp.json"`, `windows = "%APPDATA%/Code/User/mcp.json"`.

## Architecture

### Template format

One TOML file per agent:

```toml
label = "OpenCode"
format = "json"          # "json" | "toml"
key_path = ["mcp", "infigraph"]

[path]
# either a single flat string, or a per-OS table like this:
macos = "~/.config/opencode/opencode.json"
linux = "~/.config/opencode/opencode.json"
windows = "%APPDATA%/opencode/opencode.json"

[entry]
type = "local"
command = ["{{mcp_path}}", "--mcp"]
```

`{{mcp_path}}` is substituted with the absolute path to the installed `infigraph-mcp` binary at apply time. `path` may be a bare string (most agents) or the `[path]` table (VS Code today; any future agent that diverges by OS). Parse this as a `toml::Value` first and branch on `Value::String` vs `Value::Table` — do not model it as two separate typed fields, since only one is ever present and Serde's `#[serde(untagged)]` enum resolution order can silently pick the wrong variant on ambiguous input.

### Discovery

Two-tier — **not** the full three-tier grammar-plugin pattern, since which agent CLIs are installed is a per-machine fact, not a per-project one:

1. **Bundled defaults** — compiled into the binary via `include_str!`, one file per agent under `crates/infigraph-cli/resources/agent-targets/*.toml`. A small const registry (`&[(&str, &str)]`, name → contents) lists them, since `include_str!` needs literal paths (no glob).
2. **User-level override** — `~/.infigraph/agent-targets/<name>.toml`. A file matching a bundled name overrides it entirely (not merged field-by-field); a new filename adds a new agent with zero Rust changes or recompile.

### Merge engine

One generic function, `apply_agent_template(template: &AgentTemplate, mcp_path: &str) -> Result<ApplyOutcome>`:

1. Resolve `path` (substituting `~` and the per-OS table if present).
2. Read the target file if it exists.
3. **JSON format:** parse with `serde_json::from_str`. **On parse failure** (comments, trailing commas — expected for Zed's general settings file, possible for others a user hand-edited): do **not** write. Return `ApplyOutcome::Skipped { reason, manual_snippet }` — never risk corrupting a file we can't safely round-trip. No JSONC parser is introduced; this is a deliberate scope boundary (a JSONC-tolerant writer is real, separate scope if ever needed).
4. **TOML format:** existing `install_toml_target`/`uninstall_toml_target` string-splice logic is untouched (Codex only, already handles round-tripping via raw text manipulation preserving other sections).
5. Walk/create each key in `key_path`, creating intermediate objects as needed; preserve every other existing key untouched.
6. Set the final key to `entry`, with `{{mcp_path}}` substituted recursively through strings and string-array elements.
7. Write back (pretty-printed for JSON).

`ApplyOutcome` distinguishes `Applied`, `Skipped { reason, manual_snippet }` (JSONC or other unsafe-to-parse case), so `cmd_install`'s reporting can print either "Configured X" or "Skipping X — paste this by hand: ...".

### `CLAUDE_CODE_SPECIAL` removal

Claude Code's existing bespoke-path special case (`config_file == "CLAUDE_CODE_SPECIAL"` string match in `cmd_install`) becomes an ordinary template (`path = "~/.claude.json"`), removing the magic-string branch entirely — same mechanism now covers every agent uniformly.

### Uninstall symmetry

`uninstall_json_target`/`uninstall_toml_target` currently hardcode `mcpServers` — they need to read the same template's `key_path` to know what to remove (so uninstalling OpenCode correctly clears `mcp.infigraph`, not a nonexistent `mcpServers.infigraph`).

## `cmd_install` refactor (#50 groundwork)

```rust
pub(crate) enum InstallStep {
    McpRegistration,
    DocsAndRules,
    Hooks,
    Models,
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

    if steps.contains(&InstallStep::McpRegistration) {
        install_mcp_registrations(&home, &mcp_path)?;
    }
    if steps.contains(&InstallStep::DocsAndRules) {
        install_docs_and_rules(&home)?;
    }
    if steps.contains(&InstallStep::Hooks) {
        install_hooks(&home)?;
    }
    if steps.contains(&InstallStep::Models) {
        install_models(&mcp_path, &home)?;
    }
    Ok(())
}
```

- `install_mcp_registrations` — today's `AGENT_TARGETS` loop, rewritten to use `apply_agent_template` per discovered template instead of the old `(dir_name, config_file, format)` struct.
- `install_docs_and_rules` — `write_claude_md_instructions` + `write_editor_rules` + `write_reindex_command` + `print_capabilities_summary`, moved as-is.
- `install_hooks` — the existing `crate::hooks::install_*_hook` sequence + `install_claude_allowlist`, moved as-is.
- `install_models` — unchanged, moved as-is.

Existing call sites: `main.rs`'s `Commands::Install` dispatch passes `InstallStep::ALL`; `reinstall_hooks()` passes `&[InstallStep::Hooks]` (its existing behavior — it never touched MCP registration or docs). **No new CLI flag is added in this PR** — behavior is unchanged; only the internal surface now exists for a future `--mode` flag (#50) to pass a subset.

## Testing

- **Merge-engine unit tests**: create-from-empty, preserve-existing-unrelated-keys, overwrite-existing-infigraph-entry, nested `key_path` creation, JSON-parse-failure → `Skipped` (not a write), TOML path unaffected.
- **Per-template fixture tests**: for each of the 9 bundled templates, load the real bundled file (via the same `include_str!` registry production code uses — not a copy-pasted literal in the test), apply against an empty target and against a target with pre-existing unrelated keys, assert the exact expected output. This is what actually catches a future template typo, not just the generic engine.
- **Discovery override test**: a file at `~/.infigraph/agent-targets/<name>.toml` (fake `HOME`, matching this codebase's existing `Registry`-free test-isolation pattern) is used instead of the bundled default.
- **Uninstall test per divergent `key_path`**: at least OpenCode (`mcp.infigraph`) and Kiro (`mcpServers.infigraph`, but wrong old path) to confirm uninstall reads the template rather than a hardcoded key.

## Migration

Stale files at old wrong paths (e.g. `~/.opencode/config.json` from before this fix) are inert — the affected tools already don't read them (confirmed in #29's testing). This PR does not clean them up; worth a one-line callout in the PR description, not a feature.

## Hooks bundling (folds in the other half of fork issue #65)

`hooks.rs` (1827 lines on `upstream/main`) has the same hardcoding problem as `config_targets.rs`, and this PR now covers both. Confirmed by reading every `install_*_hook` function: every hook reduces to the same shape — write a bundled script to `~/.claude/hooks/<name>.sh`, register it under one or more settings.json events with an optional matcher/timeout/async, and skip if an entry for that script already exists. `install_claude_allowlist` is the one exception — it writes to `settings.local.json`'s `permissions.allow` list, not a `hooks` event at all, and stays untouched as its own function, outside this mechanism.

### Manifest format

Two sibling bundled files per hook — content and metadata kept separate, so the script gets normal shell tooling (shellcheck, syntax highlighting) instead of living inside a TOML string:

- `crates/infigraph-cli/resources/hooks/<name>.sh` — the script body (today's `*_HOOK_SCRIPT` const content, verbatim).
- `crates/infigraph-cli/resources/hooks/<name>.toml` — metadata:

```toml
label = "Edit tracker"
event = "PostToolUse"          # string, OR an array for a script registered on multiple events
matcher = "Edit|Write|NotebookEdit"   # optional -- SessionStart/SessionEnd/PreCompact take none
timeout = 5                    # default 5 if omitted
async = true                   # default false if omitted
```

`event` as an array covers `install_session_end_hook`'s existing behavior exactly: one script (`infigraph-session-end-save.sh`), registered under both `SessionEnd` and `PreCompact` with the same matcher/timeout — expressed as `event = ["SessionEnd", "PreCompact"]` in one manifest file instead of two near-identical Rust code blocks. Same parsing rule as `path` above: parse as a `toml::Value` and branch on `Value::String` vs `Value::Array`, not two separate typed fields.

### Discovery

Same two-tier pattern as agent targets: bundled (`include_str!` pairs, `&[(&str, &str, &str)]` — name, script content, manifest content) → `~/.infigraph/hooks/<name>.sh` + `<name>.toml` overrides by name or adds a new hook, no recompile needed. This is also the mechanism the fork's `install_worktree_lifecycle_hook` (not yet upstream) would adopt once this lands, rather than staying a one-off hardcoded `const` — worth noting in the PR description as a forward-looking benefit, not something this PR needs to implement.

### Generic installer

One function replaces all ten `install_*_hook` functions:

1. Write the script, `chmod +x`.
2. For each event in `event`: find an existing entry in `settings["hooks"][event]` whose `hooks[].command` contains the script's filename (the same substring-match idempotency key every existing function already uses).
3. **If found:** if its `matcher` differs from the manifest's, update it in place (self-healing on upgrade) and report "updated"; otherwise report "already configured". **If not found:** append a new `{matcher?, hooks: [{command, timeout, async?}]}` entry.

### Two deliberate behavior changes (call out explicitly in the PR description)

- **Matcher self-healing becomes uniform.** Today, only `install_enforcement_hook` updates an existing entry's `matcher` in place if it's drifted from the current hardcoded value; every other hook only checks presence/absence. The generic engine does this for all hooks — strictly additive (closes a real gap: e.g. if `session-reset`'s matcher ever needs to change in a future release, existing installs currently would *not* pick that up automatically).
- **`install_edit_tracker_hook`'s "merge into an existing matcher-containing entry" special case is dropped.** It's the one function that, instead of always creating its own entry, searches for *any* existing `PostToolUse` entry whose matcher contains `"Edit"` and appends into that entry's `hooks` array — which could mutate an entry that isn't infigraph's. The generic engine always creates/owns its own entry, matching every other hook's existing behavior. Net effect on an existing install: one extra `PostToolUse` array entry with the same `Edit|Write|NotebookEdit` matcher string as before — functionally identical (Claude Code doesn't care about duplicate matcher strings across entries), just not deduplicated at the JSON level anymore.

### Testing

- Per-manifest fixture test (one per bundled hook): apply against empty `settings.json`, assert exact expected entry/entries.
- Idempotency test: apply twice, second run reports "already configured", no duplicate entries.
- Matcher self-heal test: apply, hand-edit the resulting matcher, re-apply, assert it's restored and reported as "updated".
- Multi-event test: the session-end manifest (`event = ["SessionEnd", "PreCompact"]`) produces both entries from one apply call.
- Discovery override test: same pattern as agent targets, fake `HOME`.

## Out of scope

- No `--mode` / install-tier flag or UX (that's #50 itself, a separate follow-up PR once this groundwork lands).
- No JSONC-tolerant parser (Zed's embedded-settings case bails with instructions instead).
- No project-level (three-tier) discovery for agent targets or hooks.
- `install_claude_allowlist` is untouched (different settings file, different section — not a `hooks` event).
