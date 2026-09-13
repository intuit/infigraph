# Agent Integration Plugins

Infigraph supports data-driven agent integrations for `infigraph install`/`infigraph uninstall` — bundled config fragments, hook scripts, and docs for each supported AI coding agent, applied to the user's home directory via one of five declarative strategies. No recompilation needed to add a new agent, fix a wrong config path, or override bundled content for one machine.

## Architecture

```
resources/integrations/<agent>/**        ~/.infigraph/integrations/<agent>/**
       │  (compiled in via build.rs)            │  (user override, same layout)
       └───────────────────┬─────────────────────┘
                            ▼
              discover_artifacts(bundled, user_override_dir, mcp_path)
                            │
                ┌───────────┴────────────┐
                │                        │
     <agent>/config.toml        every other file, not a
     [[artifact]] entries       manifest, not manifest-claimed
     (explicit path/strategy)   (strategy inferred from extension)
                │                        │
                └───────────┬────────────┘
                            ▼
                    ResolvedArtifact {
                      target_relative_path | resolver,
                      strategy, content, ...
                    }
                            │
                            ▼
              apply_resolved_artifact() / remove_resolved_artifact()
                            │
                            ▼
                 json_deep_merge | overwrite | marker_delimited
                 toml_section    | json_key_path
                            │
                            ▼
                  ~/.claude/settings.json, ~/.codex/config.toml,
                  ~/.cursor/mcp.json, ...
```

## How It Works

1. An integration is a directory under `resources/integrations/` — usually a `config.toml` manifest plus one or more content files, though the manifest is optional for simple cases (see "Convention-Based Artifacts" below).
2. Infigraph discovers integrations at `infigraph install`/`infigraph uninstall` time from two locations:
   - `resources/integrations/<agent>/**` — bundled, compiled into the binary via `build.rs`
   - `~/.infigraph/integrations/<agent>/**` — user-level override, same directory layout
3. The two sources merge into one flat `relative_path → content` map; a user-override file at the same relative path replaces the bundled one, and a wholly new `<agent>/` directory the user adds is picked up with zero code changes.
4. Discovery runs in two passes over that merged map:
   - **Pass 1** — every `<agent>/config.toml`'s `[[artifact]]` entries, each explicit about its destination path (or a resolver), strategy, and content.
   - **Pass 2** — every remaining file that isn't a manifest, isn't already claimed as a manifest entry's `content_file`, and isn't under `shared/`: strategy is inferred from its extension, and its destination path is the bundled path with the leading `<agent>/` segment stripped (the bundled subtree is authored to already mirror the real path under `$HOME`).
5. Each resulting `ResolvedArtifact` is applied via its strategy on `infigraph install` (idempotent — re-running never duplicates), and reversed via the matching `remove_*` on `infigraph uninstall`.

## Directory Structure

```
resources/integrations/
  claude-code/
    config.toml                          # manifest (label + 2 artifacts)
    .claude.json                         # convention-based (no manifest entry needed)
    .claude/
      settings.json                      # convention-based
      hooks/
        infigraph-enforce.sh             # convention-based
        infigraph-session-save.sh        # ...
  codex/
    config.toml                          # manifest — .toml destinations always need one
    mcp-section.toml                     # referenced via content_file
  vscode/
    config.toml                          # manifest — resolver-driven (OS/profile-dependent path)
    mcp-fragment.json                    # referenced via content_file
    resolve-vscode-path.py               # referenced via resolver
  shared/
    agents.md                            # referenced via content_file from multiple agents
    skills/infigraph-reindex/SKILL.md    # ditto
  .gitkeep

~/.infigraph/integrations/               # user override, same layout, optional
  claude-code/
    .claude/hooks/infigraph-enforce.sh   # replaces the bundled version at this path
  my-custom-agent/
    .custom/mcp.json                     # a wholly new integration, zero code changes
```

A bundled file's location below its `<agent>/` directory is authored to already mirror its real destination under `$HOME` — `claude-code/.claude/hooks/x.sh` strips to `.claude/hooks/x.sh`; `claude-code/.claude.json` strips to `.claude.json`, since that file's real destination has no further nesting. `shared/` content is never applied directly — it's only reachable via another integration's `content_file` reference (see below).

## config.toml Manifest Format

```toml
label = "Claude Code"                     # optional -- display name; defaults to the directory name

[[artifact]]
path = ".claude/CLAUDE.md"                # destination relative to $HOME (omit if using "resolver" instead)
strategy = "marker_delimited"             # one of the five strategies (see below)
start = "<!-- infigraph-primary-search -->"   # marker_delimited only
end = "<!-- /infigraph-primary-search -->"    # marker_delimited only
content_file = "../shared/agents.md"      # path relative to this manifest's own directory; may escape with "../"
# key_path = ["mcp_servers", "infigraph"] # toml_section / json_key_path only
# resolver = ["./resolve-x-path.py"]      # instead of "path", for OS/profile-dependent destinations
```

### Field Reference

| Field | Required | Description |
|-------|----------|--------------|
| `path` | one of `path`/`resolver` | Destination relative to `$HOME`. |
| `resolver` | one of `path`/`resolver` | Command to compute the destination (and optionally content) at apply time — see "The Resolver Escape Hatch". |
| `strategy` | Yes | `json_deep_merge`, `overwrite`, `marker_delimited`, `toml_section`, or `json_key_path`. |
| `content_file` | No | Path to this artifact's content, relative to the manifest's own directory. Omit for artifacts with no static content (rare). |
| `start` / `end` | `marker_delimited` only | Delimiter strings bounding the managed block. |
| `key_path` | `toml_section` / `json_key_path` only | Dotted path to the owned section/key, as an array (`["mcp_servers", "infigraph"]`). |

### Convention-Based Artifacts (No Manifest Needed)

A file under `<agent>/` with no `[[artifact]]` entry referencing it is still applied, strategy inferred from its extension:

- `.json` → `json_deep_merge`
- anything else (`.sh`, `.md`, `.mdc`, ...) → `overwrite`
- `.toml` → **never** convention-based; always requires an explicit manifest entry with `strategy = "toml_section"` and a `key_path` — a surgical raw-text section splice can't be inferred from extension alone the way JSON's structural merge can, and blindly parsing/reserializing a hand-maintained TOML file risks stripping the user's own comments.

This is why `gemini-cli`, `opencode`, `aider`, `kiro`, and `github-copilot-cli` ship with **no `config.toml` at all** — a single bundled `.json` fragment is entirely convention-based.

## The Five Strategies

| Strategy | Destination shape | Apply | Remove |
|----------|-------------------|-------|--------|
| `json_deep_merge` | JSON file, possibly hand-maintained | Deep-merges the fragment in, preserving unrelated keys at any depth. An existing array entry is recognized as "ours" (and replaced, not duplicated) if it either contains the substring `"infigraph"` or exactly equals one of the fragment's own entries — so re-applying never duplicates, even after the entry's shape has drifted. | Strips exactly the keys/array-entries the fragment owns; cascades key deletion when a container becomes empty (`mcpServers.infigraph` disappears entirely, not left behind as `{}`). |
| `overwrite` | Whole file (scripts, docs, simple JSON) | Replaces the file's bytes exactly. If the destination path has a `hooks` path segment, the written file is also `chmod 0o755` — hook scripts run via shebang through the user's shell and a non-executable one silently never fires. | Deletes the file. |
| `marker_delimited` | A block inside a larger file the user also edits (e.g. `CLAUDE.md`) | Replaces the region between `start`/`end` markers, inserting it if absent. Refuses to guess if `start` is present but `end` is missing or damaged. | Strips the marked block, leaving the rest of the file untouched. |
| `toml_section` | A hand-maintained TOML file (e.g. `~/.codex/config.toml`) | Locates a table-header line matching `key_path` exactly (line-based, not a substring search — so a header mentioned inside a comment or a quoted string is never matched), replaces from that line through the next table header or EOF. | Removes only the matched section, preserving every other section and comment. |
| `json_key_path` | JSON file, single owned key | Sets `content` at the nested path `key_path` describes, creating intermediate objects as needed. | Removes the leaf at `key_path`, keeping siblings. |

Every strategy's `apply_*` is idempotent — re-running `infigraph install` never duplicates an entry, whether the destination didn't exist, already had the entry, or had it in a stale shape.

## Template Substitution

Content files may contain `{{mcp_path}}`, substituted with the real path to the `infigraph-mcp` binary at apply time — escaped for the destination format (`json_deep_merge`/`json_key_path` → JSON string escaping, `toml_section` → TOML string escaping; `overwrite`/`marker_delimited` content is never substituted, since scripts and docs don't carry the MCP path as a quoted literal).

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

## The Resolver Escape Hatch

Some agents (VS Code, Zed) keep their MCP config at an OS- or profile-dependent path that can't be expressed as a single static string. For these, a manifest entry declares `resolver` instead of `path`:

```toml
[[artifact]]
strategy = "json_deep_merge"
resolver = ["python3", "./resolve-vscode-path.py"]
content_file = "mcp-fragment.json"
```

`resolver` is a full command array, same shape as pipeline plugins' `command` field — the **last** element is the script (looked up in the bundled/user-override registry, materialized to a temp file, and made executable), and every element before it is a literal command prefix, prepended exactly as written at invocation time. Nothing is inferred from the script's extension: `resolver = ["./resolve.sh"]` executes it directly (relies on the shebang, Unix-only); `resolver = ["node", "./resolve.js"]` would run it under Node on any OS. The one exception is a first prefix element of exactly `"python"` or `"python3"` — normalized to whichever binary actually exists under that name for the current OS (`python3` on Unix, `python` on Windows, which ships no `python3` alias) — since that Unix/Windows naming split is common enough to be worth handling once rather than pushing onto every manifest author.

At apply/remove time, the resolver script is materialized to a temp directory, made executable, and run with the standard resolver-contract JSON on stdin:

```json
{"mcp_path": "/usr/local/bin/infigraph-mcp", "os": "macos", "home": "/Users/alice"}
```

It responds on stdout with one of:

```json
{"status": "ok", "data": {"path": "/Users/alice/Library/Application Support/Zed/settings.json"}}
```
```json
{"status": "ok", "data": {"path": "...", "content": {"command": "...", "args": ["--mcp"]}}}
```
```json
{"status": "skip", "message": "Zed is not installed"}
```
```json
{"status": "error", "message": "could not detect active VS Code profile"}
```

`data.content`, if present, replaces the manifest's `content_file` entirely — useful when the resolver itself needs to compute the fragment (e.g. discovering the active profile directory). If omitted, the manifest's own `content_file` (template-substituted as usual) is used against the resolver-supplied path.

A `resolver` with no prefix (`resolver = ["./resolve.sh"]`) executes the script directly, which is Unix-correct via its shebang and a clear, immediate spawn failure on Windows rather than a silent wrong-path run — the manifest author's job to avoid by declaring an explicit interpreter for anything that needs cross-platform support, exactly as it's the pipeline plugin author's job when writing `command`.

## Supported Agents

| Agent | Bundled path(s) | Strategy | Manifest? |
|-------|------------------|----------|-----------|
| Claude Code | `.claude.json`, `.claude/settings.json`, `.claude/hooks/*.sh`, `.claude/CLAUDE.md`, `.claude/skills/infigraph-reindex/SKILL.md` | `json_deep_merge`, `overwrite` (hooks), `marker_delimited` (CLAUDE.md) | Yes |
| Codex | `.codex/config.toml`, `.codex/skills/infigraph-reindex/SKILL.md` | `toml_section`, `overwrite` | Yes |
| Cursor | `.cursor/mcp.json`, `.cursor/rules/infigraph.mdc` | `json_deep_merge`, `overwrite` | Yes |
| Windsurf | `.codeium/windsurf/mcp_config.json`, `.windsurf/rules/infigraph.md` | `json_deep_merge`, `overwrite` | Yes |
| VS Code | resolver-determined (per-OS user settings path) | `json_deep_merge` | Yes (resolver) |
| Zed | resolver-determined (per-OS settings path) | `json_key_path` | Yes (resolver) |
| Gemini CLI | `.gemini/settings.json` | `json_deep_merge` | No (convention) |
| OpenCode | `.config/opencode/opencode.json` | `json_deep_merge` | No (convention) |
| Aider | `.aider/mcp.json` | `json_deep_merge` | No (convention) |
| Kiro | `.kiro/settings/mcp.json` | `json_deep_merge` | No (convention) |
| GitHub Copilot CLI | `.copilot/mcp-config.json` | `json_deep_merge` | No (convention) |

## Writing a Custom Integration

### Step 1: Create the directory

```bash
mkdir -p ~/.infigraph/integrations/my-agent
```

### Step 2: Decide manifest vs. convention

If your agent reads a single JSON MCP config file and nothing else needs a manifest at all:

```bash
mkdir -p ~/.infigraph/integrations/my-agent/.my-agent
cat > ~/.infigraph/integrations/my-agent/.my-agent/mcp.json <<'EOF'
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
EOF
```

That's it — no `config.toml`, the `.json` extension is enough for `infigraph install` to pick this up as `json_deep_merge` and write it to `~/.my-agent/mcp.json`.

For anything needing a TOML destination, a marker-delimited doc block, or multiple artifacts, write `config.toml`:

```toml
label = "My Agent"

[[artifact]]
path = ".my-agent/config.toml"
strategy = "toml_section"
key_path = ["mcp", "infigraph"]
content_file = "mcp-section.toml"
```

```toml
# ~/.infigraph/integrations/my-agent/mcp-section.toml
command = "{{mcp_path}}"
args = ["--mcp"]
```

### Step 3: Test

```bash
infigraph install    # writes to the real destination under $HOME
infigraph uninstall   # verify it cleanly reverses, preserving unrelated content
```

To test against a fake home without touching your real config, the artifact engine's own test suite (`crates/infigraph-cli/src/artifacts/mod.rs`, `integration_tests`) shows the pattern: call `discover_artifacts`/`apply_resolved_artifact` directly against a `tempfile::tempdir()`.

### Step 4: Verify idempotency

Run `infigraph install` twice — the destination file must not gain a duplicate entry. Then run `infigraph uninstall` and confirm any other content in the destination file (an unrelated `mcpServers` entry, other TOML sections) survived untouched.

## Example: Adding a New Agent End-to-End

Say "AcmeCode" reads MCP servers from `~/.acme/settings.json` under a `mcp` key shaped like `{"mcp": {"infigraph": {"command": "...", "args": [...]}}}`, and also wants the shared `agents.md` instructions written to `~/.acme/AGENTS.md`.

1. `mkdir -p resources/integrations/acmecode/.acme`
2. Since the MCP file is plain JSON with no other special handling needed, `.acme/settings.json` alone (convention-based) would work for the MCP part — but since we also want `AGENTS.md` (which needs no marker/section splice, just an overwrite) and a `label`, write a manifest instead for clarity:

   ```toml
   # resources/integrations/acmecode/config.toml
   label = "AcmeCode"

   [[artifact]]
   path = ".acme/settings.json"
   strategy = "json_deep_merge"
   content_file = "mcp-fragment.json"

   [[artifact]]
   path = ".acme/AGENTS.md"
   strategy = "overwrite"
   content_file = "../shared/agents.md"
   ```

3. `resources/integrations/acmecode/mcp-fragment.json`:
   ```json
   {"mcp":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
   ```
4. Run `cargo test -p infigraph-cli --bin infigraph artifacts::` — the existing `discovers_convention_based_json_artifact_from_bundled`-style tests will exercise the new integration once you add a matching test, following the pattern in `crates/infigraph-cli/src/artifacts/mod.rs`'s `integration_tests` module.
5. `cargo build -p infigraph-cli --bin infigraph && infigraph install` against a scratch `$HOME` (`HOME=/tmp/test-home infigraph install`) and confirm `~/.acme/settings.json` and `~/.acme/AGENTS.md` land correctly, then `infigraph uninstall` and confirm both are cleanly removed.
