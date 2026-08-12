# Data-Driven Integration Artifacts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `infigraph-cli`'s hardcoded per-agent MCP config writers (`config_targets.rs`), hardcoded hook-install functions (`hooks.rs`), and hardcoded docs/rules/reindex writers (`install.rs`) with one data-driven artifact engine, bundled content for all 13 supported integrations, and `cmd_install`/`cmd_uninstall` rewired onto it — fixing upstream issue #29 (wrong MCP config shape/path for several agents) and laying groundwork for #50 (install modes), per `docs/superpowers/specs/2026-08-09-agent-target-templates-design.md`.

**Architecture:** Tasks 1-12 build a new, fully self-contained `crates/infigraph-cli/src/artifacts/` engine (manifest parsing, template substitution, five artifact strategies plus their uninstall inverses, convention-based classification, resolver subprocess IPC, `build.rs` bundled-resource embedding, two-tier discovery, `InstallStep` classification) — purely additive, touching no existing code. Tasks 13 onward populate `crates/infigraph-cli/resources/integrations/` with real content for every integration, then rewire `cmd_install`/`cmd_uninstall` onto the engine and delete the code it replaces.

**Tech Stack:** Rust, `serde`/`serde_json` (already a dependency), `toml` and `tempfile` (both promoted from dev-dependency to a real dependency — `tempfile` is needed at runtime, not just in tests, to materialize a bundled resolver script to a real file before spawning it), `anyhow` for error handling.

## Global Constraints

- Tasks 1-12 create only new files under `crates/infigraph-cli/src/artifacts/` or `crates/infigraph-cli/build.rs`, plus two small edits to existing files (`Cargo.toml` dependency promotion, one `mod artifacts;` line in `main.rs`) — no other existing code changes until Task 13.
- `.toml` destination files are **never** convention-based — `infer_strategy` (Task 7) must return `None` for any `.toml` path, forcing a manifest entry.
- The array-entry ownership rule for `json_deep_merge`/`json_key_path` is: an existing array entry is "ours" (subject to replacement) if **either** its serialized JSON contains the substring `infigraph`, **or** it exactly equals one of the fragment's own entries. (The substring-only rule from the design spec has a duplication bug for scalar-ish owned arrays like `args: ["--mcp"]`, which never contains the substring "infigraph" — see Task 4's step 1 for the failing-test proof. The exact-match fallback closes it without weakening self-healing for path-bearing entries.)
- Every `apply_*` function returns `Result<ApplyOutcome>` (`Written` or `Skipped { reason, manual_snippet }`) — never a bare `Result<()>` — so `cmd_install` (Task 19) can report skips instead of silently swallowing them.
- Bundled content embedded via `build.rs` must be re-derivable from `crates/infigraph-cli/resources/integrations/` alone — no hand-maintained duplicate list of filenames anywhere else in the crate.
- Every bundled MCP-registration fragment (`.json` or `.toml`) uses the literal placeholder `{{mcp_path}}` for the `infigraph-mcp` binary path — never a hardcoded path — substituted at discovery/apply time via `template::substitute_mcp_path` (Task 3).
- Hook scripts (Task 14) are copied byte-for-byte from their current `hooks.rs` string constants — no behavioral changes to any hook's logic in this plan; only *how* they reach `~/.claude/hooks/` changes (bundled file + `overwrite` convention, not a Rust `std::fs::write` call).
- Run `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` before every commit in this plan (per this repo's CI gate) — fix any warning the new code introduces before moving to the next task.
- This work is destined for **upstream** (`intuit/infigraph`), not the fork — do not push or open a PR without explicit user approval, regardless of how complete the plan is.

---

## Task 1: Scaffolding — `toml` dependency, empty resources dir, `artifacts` module skeleton

**Files:**
- Modify: `crates/infigraph-cli/Cargo.toml`
- Create: `crates/infigraph-cli/src/artifacts/mod.rs`
- Create: `crates/infigraph-cli/resources/integrations/.gitkeep`
- Modify: `crates/infigraph-cli/src/main.rs:1` (add `mod artifacts;`)

**Interfaces:**
- Produces: an empty `pub(crate) mod artifacts { }` compiling cleanly, ready for submodules in later tasks.

- [ ] **Step 1: Promote `toml` and `tempfile` to real dependencies**

In `crates/infigraph-cli/Cargo.toml`, move both the `toml = "0.8"` and `tempfile = "3"` lines out of `[dev-dependencies]` into `[dependencies]`. After the edit, `[dependencies]` gains both lines and `[dev-dependencies]` is empty (Task 9 onward removes the now-empty `[dev-dependencies]` table if `cargo fmt`/clippy flag it; otherwise leave it, an empty table is harmless). `tempfile` needs to be a real dependency because Task 12 uses it at runtime (not just in tests) to materialize a bundled resolver script to a real file before spawning it as a subprocess.

- [ ] **Step 2: Create the empty bundled-resources directory**

```bash
mkdir -p /Users/pmouli/GitHub.nosync/active/rust/infigraph/crates/infigraph-cli/resources/integrations
touch /Users/pmouli/GitHub.nosync/active/rust/infigraph/crates/infigraph-cli/resources/integrations/.gitkeep
```

Tasks 1-12 do not populate real integration content (that starts at Task 13) — the directory only needs to exist so `build.rs` (Task 9) has something to walk without erroring, and so `git` tracks the empty directory via `.gitkeep`.

- [ ] **Step 3: Create the `artifacts` module skeleton**

Write `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
//! Data-driven integration artifacts: bundled config fragments, hook scripts,
//! and docs for every supported agent/editor, applied via one of five
//! strategies. See docs/superpowers/specs/2026-08-09-agent-target-templates-design.md.

mod convention;
mod discovery;
mod manifest;
mod resolver;
mod step;
mod strategy;
mod template;
```

- [ ] **Step 4: Add the placeholder submodules so the crate compiles**

Each of the six files below is empty except a doc comment — later tasks fill them in one at a time. Write each:

`crates/infigraph-cli/src/artifacts/manifest.rs`:
```rust
// Filled in by Task 2.
```

`crates/infigraph-cli/src/artifacts/template.rs`:
```rust
// Filled in by Task 3.
```

`crates/infigraph-cli/src/artifacts/strategy.rs`:
```rust
// Filled in by Tasks 4-6.
```

`crates/infigraph-cli/src/artifacts/convention.rs`:
```rust
// Filled in by Task 7.
```

`crates/infigraph-cli/src/artifacts/resolver.rs`:
```rust
// Filled in by Task 8.
```

`crates/infigraph-cli/src/artifacts/discovery.rs`:
```rust
// Filled in by Task 9.
```

`crates/infigraph-cli/src/artifacts/step.rs`:
```rust
// Filled in by Task 10.
```

- [ ] **Step 5: Register the module in `main.rs`**

In `crates/infigraph-cli/src/main.rs`, add `mod artifacts;` as the first line (before `mod agent;`), so the file starts:

```rust
mod artifacts;
mod agent;
mod analysis_commands;
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo build -p infigraph-cli`
Expected: builds successfully with no errors (unused-module warnings are fine at this stage — every submodule is genuinely empty).

- [ ] **Step 7: Commit**

```bash
git add crates/infigraph-cli/Cargo.toml crates/infigraph-cli/src/artifacts crates/infigraph-cli/src/main.rs crates/infigraph-cli/resources/integrations/.gitkeep
git commit -m "feat(cli): scaffold artifacts module for data-driven integration engine"
```

---

## Task 2: Manifest parsing (`config.toml` → `IntegrationManifest`)

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/manifest.rs`

**Interfaces:**
- Produces:
  - `pub(crate) struct IntegrationManifest { pub label: Option<String>, pub artifacts: Vec<ArtifactEntry> }`
  - `pub(crate) struct ArtifactEntry { pub path: String, pub strategy: String, pub start: Option<String>, pub end: Option<String>, pub content_file: Option<String>, pub key_path: Option<Vec<String>>, pub resolver: Option<Vec<String>> }` — **Task 12 later changes `path` to `Option<String>`**; a resolver-only entry (VS Code, Zed) has no static path, and requiring one here would reject the design spec's own Zed example manifest.
  - `pub(crate) fn parse_manifest(content: &str) -> anyhow::Result<IntegrationManifest>`

- [ ] **Step 1: Write the failing tests**

Append to `crates/infigraph-cli/src/artifacts/manifest.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest_with_one_artifact() {
        let toml = r#"
label = "Claude Code"

[[artifact]]
path = ".claude/CLAUDE.md"
strategy = "marker_delimited"
start = "<!-- infigraph-primary-search -->"
end = "<!-- /infigraph-primary-search -->"
content_file = "../shared/agents.md"
"#;
        let manifest = parse_manifest(toml).expect("should parse");
        assert_eq!(manifest.label.as_deref(), Some("Claude Code"));
        assert_eq!(manifest.artifacts.len(), 1);
        let a = &manifest.artifacts[0];
        assert_eq!(a.path, ".claude/CLAUDE.md");
        assert_eq!(a.strategy, "marker_delimited");
        assert_eq!(a.start.as_deref(), Some("<!-- infigraph-primary-search -->"));
        assert_eq!(a.end.as_deref(), Some("<!-- /infigraph-primary-search -->"));
        assert_eq!(a.content_file.as_deref(), Some("../shared/agents.md"));
        assert!(a.key_path.is_none());
        assert!(a.resolver.is_none());
    }

    #[test]
    fn parses_manifest_with_multiple_artifacts_and_no_label() {
        let toml = r#"
[[artifact]]
path = ".codex/config.toml"
strategy = "toml_section"
key_path = ["mcp_servers", "infigraph"]
content_file = "mcp-section.toml"

[[artifact]]
path = ".codex/skills/infigraph-reindex/SKILL.md"
strategy = "overwrite"
content_file = "../shared/skills/infigraph-reindex/SKILL.md"
"#;
        let manifest = parse_manifest(toml).expect("should parse");
        assert!(manifest.label.is_none());
        assert_eq!(manifest.artifacts.len(), 2);
        assert_eq!(
            manifest.artifacts[0].key_path,
            Some(vec!["mcp_servers".to_string(), "infigraph".to_string()])
        );
        assert_eq!(manifest.artifacts[1].strategy, "overwrite");
    }

    #[test]
    fn parses_resolver_field() {
        let toml = r#"
[[artifact]]
path = "unused-for-resolver-based-artifacts"
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]
"#;
        let manifest = parse_manifest(toml).expect("should parse");
        assert_eq!(
            manifest.artifacts[0].resolver,
            Some(vec!["./resolve-zed-path.sh".to_string()])
        );
    }

    #[test]
    fn rejects_malformed_toml() {
        let result = parse_manifest("this is not [ valid toml");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_artifact_missing_required_path() {
        let toml = r#"
[[artifact]]
strategy = "overwrite"
"#;
        let result = parse_manifest(toml);
        assert!(result.is_err(), "path is required, should fail to parse");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::manifest -- --nocapture`
Expected: compile error (`IntegrationManifest`, `ArtifactEntry`, `parse_manifest` don't exist yet).

- [ ] **Step 3: Implement the manifest types and parser**

Replace the contents of `crates/infigraph-cli/src/artifacts/manifest.rs` (keeping the `#[cfg(test)] mod tests { ... }` block from Step 1 at the end) with:

```rust
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IntegrationManifest {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(rename = "artifact", default)]
    pub artifacts: Vec<ArtifactEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ArtifactEntry {
    pub path: String,
    pub strategy: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub content_file: Option<String>,
    #[serde(default)]
    pub key_path: Option<Vec<String>>,
    #[serde(default)]
    pub resolver: Option<Vec<String>>,
}

pub(crate) fn parse_manifest(content: &str) -> Result<IntegrationManifest> {
    toml::from_str(content).context("failed to parse integration manifest (config.toml)")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::manifest -- --nocapture`
Expected: all 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/manifest.rs
git commit -m "feat(cli): parse integration config.toml manifests"
```

---

## Task 3: Template substitution (`{{mcp_path}}`)

Every MCP-registration content fragment (JSON or TOML) needs the runtime-detected `infigraph-mcp` binary path baked in — this can't be static bundled content. Fragments contain a literal `{{mcp_path}}` placeholder; this task substitutes it with the actual path, escaped correctly for the destination format, before the fragment is parsed by any strategy.

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/template.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub(crate) enum TemplateFormat { Json, Toml }`
  - `pub(crate) fn substitute_mcp_path(content: &str, mcp_path: &str, format: TemplateFormat) -> String`

- [ ] **Step 1: Write the failing tests**

Write `crates/infigraph-cli/src/artifacts/template.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_substitution_escapes_backslashes_and_quotes() {
        let content = r#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}"#;
        let result = substitute_mcp_path(content, r"C:\Users\foo\infigraph-mcp.exe", TemplateFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&result).expect("must still be valid JSON");
        assert_eq!(
            parsed["mcpServers"]["infigraph"]["command"],
            r"C:\Users\foo\infigraph-mcp.exe"
        );
    }

    #[test]
    fn json_substitution_leaves_plain_unix_path_untouched() {
        let content = r#"{"command":"{{mcp_path}}"}"#;
        let result = substitute_mcp_path(content, "/usr/bin/infigraph-mcp", TemplateFormat::Json);
        assert_eq!(result, r#"{"command":"/usr/bin/infigraph-mcp"}"#);
    }

    #[test]
    fn toml_substitution_escapes_backslashes_and_quotes() {
        let content = "command = \"{{mcp_path}}\"\nargs = [\"--mcp\"]\n";
        let result = substitute_mcp_path(content, r"C:\Users\foo\infigraph-mcp.exe", TemplateFormat::Toml);
        let parsed: toml::Value = toml::from_str(&format!("[x]\n{result}")).expect("must still be valid TOML");
        assert_eq!(
            parsed["x"]["command"].as_str(),
            Some(r"C:\Users\foo\infigraph-mcp.exe")
        );
    }

    #[test]
    fn substitution_handles_multiple_occurrences() {
        let content = r#"{"a":"{{mcp_path}}","b":"{{mcp_path}}"}"#;
        let result = substitute_mcp_path(content, "/bin/x", TemplateFormat::Json);
        assert_eq!(result, r#"{"a":"/bin/x","b":"/bin/x"}"#);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::template -- --nocapture`
Expected: compile error (`substitute_mcp_path`, `TemplateFormat` don't exist yet).

- [ ] **Step 3: Implement template substitution**

Prepend to `crates/infigraph-cli/src/artifacts/template.rs` (above the `#[cfg(test)]` block):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateFormat {
    Json,
    Toml,
}

/// Escapes `mcp_path` for safe embedding inside a JSON or TOML string literal,
/// then replaces every `{{mcp_path}}` occurrence in `content` with it.
pub(crate) fn substitute_mcp_path(content: &str, mcp_path: &str, format: TemplateFormat) -> String {
    let escaped = match format {
        // JSON and TOML basic-string escaping happen to coincide for the two
        // characters a filesystem path can contain that matter here.
        TemplateFormat::Json | TemplateFormat::Toml => {
            mcp_path.replace('\\', "\\\\").replace('"', "\\\"")
        }
    };
    content.replace("{{mcp_path}}", &escaped)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::template -- --nocapture`
Expected: all 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/template.rs
git commit -m "feat(cli): add {{mcp_path}} template substitution for bundled fragments"
```

---

## Task 4: `json_deep_merge` and `overwrite` strategies

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/strategy.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks (pure `std`/`serde_json`).
- Produces:
  - `pub(crate) enum ApplyOutcome { Written, Skipped { reason: String, manual_snippet: String } }`
  - `pub(crate) fn merge_json(target: &mut serde_json::Value, fragment: &serde_json::Value)`
  - `pub(crate) fn apply_json_deep_merge(target_path: &std::path::Path, fragment_content: &str) -> anyhow::Result<ApplyOutcome>`
  - `pub(crate) fn apply_overwrite(target_path: &std::path::Path, content: &[u8]) -> anyhow::Result<ApplyOutcome>`
  - `pub(crate) fn remove_json_deep_merge(target_path: &std::path::Path, fragment_content: &str) -> anyhow::Result<bool>`
  - `pub(crate) fn remove_overwrite(target_path: &std::path::Path) -> anyhow::Result<bool>`

- [ ] **Step 1: Write the failing tests, including the duplication-bug proof**

Write `crates/infigraph-cli/src/artifacts/strategy.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn json_deep_merge_creates_from_empty() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mcp.json");
        let outcome = apply_json_deep_merge(
            &target,
            r#"{"mcpServers":{"infigraph":{"command":"/bin/infigraph-mcp","args":["--mcp"]}}}"#,
        )
        .unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));
        let v = read_json(&target);
        assert_eq!(v["mcpServers"]["infigraph"]["command"], "/bin/infigraph-mcp");
        assert_eq!(v["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }

    #[test]
    fn json_deep_merge_preserves_unrelated_keys_at_any_depth() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mcp.json");
        std::fs::write(
            &target,
            r#"{"mcpServers":{"other":{"command":"other-cmd"}},"topLevel":"kept","nested":{"deep":{"value":42}}}"#,
        )
        .unwrap();

        apply_json_deep_merge(
            &target,
            r#"{"mcpServers":{"infigraph":{"command":"/bin/infigraph-mcp","args":["--mcp"]}}}"#,
        )
        .unwrap();

        let v = read_json(&target);
        assert_eq!(v["topLevel"], "kept");
        assert_eq!(v["nested"]["deep"]["value"], 42);
        assert_eq!(v["mcpServers"]["other"]["command"], "other-cmd");
        assert_eq!(v["mcpServers"]["infigraph"]["command"], "/bin/infigraph-mcp");
    }

    #[test]
    fn json_deep_merge_reapply_does_not_duplicate_scalar_owned_array() {
        // Regression test for the substring-only ownership rule's duplication
        // bug: "args": ["--mcp"] never contains the substring "infigraph", so
        // a naive filter-then-append would duplicate it on every reinstall.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mcp.json");
        let fragment = r#"{"mcpServers":{"infigraph":{"command":"/bin/infigraph-mcp","args":["--mcp"]}}}"#;

        apply_json_deep_merge(&target, fragment).unwrap();
        apply_json_deep_merge(&target, fragment).unwrap();
        apply_json_deep_merge(&target, fragment).unwrap();

        let v = read_json(&target);
        let args = v["mcpServers"]["infigraph"]["args"].as_array().unwrap();
        assert_eq!(args.len(), 1, "args should not duplicate across reinstalls, got {:?}", args);
        assert_eq!(args[0], "--mcp");
    }

    #[test]
    fn json_deep_merge_array_ownership_self_heals_via_substring_match() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(
            &target,
            r#"{"hooks":{"PreToolUse":[{"matcher":"STALE","hooks":[{"command":"/home/x/.claude/hooks/infigraph-enforce.sh"}]}]}}"#,
        )
        .unwrap();

        let fragment = r#"{"hooks":{"PreToolUse":[{"matcher":"Grep|Glob","hooks":[{"command":"/home/x/.claude/hooks/infigraph-enforce.sh"}]}]}}"#;
        apply_json_deep_merge(&target, fragment).unwrap();

        let v = read_json(&target);
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "stale entry should be replaced, not duplicated");
        assert_eq!(arr[0]["matcher"], "Grep|Glob");
    }

    #[test]
    fn json_deep_merge_array_ownership_leaves_other_tools_entries_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(
            &target,
            r#"{"hooks":{"PreToolUse":[{"matcher":"SomeOtherTool","hooks":[{"command":"/usr/local/bin/other-tool-hook.sh"}]}]}}"#,
        )
        .unwrap();

        let fragment = r#"{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"command":"/home/x/.claude/hooks/infigraph-enforce.sh"}]}]}}"#;
        apply_json_deep_merge(&target, fragment).unwrap();

        let v = read_json(&target);
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "other tool's entry must survive alongside ours");
        assert!(arr.iter().any(|e| e["matcher"] == "SomeOtherTool"));
        assert!(arr.iter().any(|e| e["matcher"] == "Grep"));
    }

    #[test]
    fn json_deep_merge_handles_array_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(&target, r#"{"hooks":{}}"#).unwrap();

        let fragment = r#"{"hooks":{"PreToolUse":[{"matcher":"Grep","hooks":[{"command":"infigraph-enforce.sh"}]}]}}"#;
        apply_json_deep_merge(&target, fragment).unwrap();

        let v = read_json(&target);
        assert_eq!(v["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn json_deep_merge_parse_failure_returns_skipped_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        // Trailing comma + comment: invalid strict JSON, plausible hand-edited JSONC.
        std::fs::write(&target, "{\n  // a comment\n  \"foo\": \"bar\",\n}\n").unwrap();
        let before = std::fs::read_to_string(&target).unwrap();

        let outcome = apply_json_deep_merge(&target, r#"{"mcpServers":{"infigraph":{}}}"#).unwrap();
        match outcome {
            ApplyOutcome::Skipped { reason, manual_snippet } => {
                assert!(reason.contains("not valid JSON"));
                assert!(manual_snippet.contains("infigraph"));
            }
            ApplyOutcome::Written => panic!("must not write on parse failure"),
        }
        let after = std::fs::read_to_string(&target).unwrap();
        assert_eq!(before, after, "file must be untouched on parse failure");
    }

    #[test]
    fn overwrite_writes_bytes_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("hooks").join("infigraph-enforce.sh");
        let outcome = apply_overwrite(&target, b"#!/usr/bin/env bash\necho hi\n").unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"#!/usr/bin/env bash\necho hi\n"
        );
    }

    #[test]
    fn overwrite_replaces_prior_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("infigraph.mdc");
        std::fs::write(&target, "old content").unwrap();
        apply_overwrite(&target, b"new content").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new content");
    }

    #[test]
    fn remove_json_deep_merge_removes_owned_key_and_preserves_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mcp.json");
        let fragment = r#"{"mcpServers":{"infigraph":{"command":"/bin/infigraph-mcp","args":["--mcp"]}}}"#;
        apply_json_deep_merge(&target, fragment).unwrap();
        std::fs::write(
            &target,
            serde_json::to_string_pretty(&{
                let mut v = read_json(&target);
                v["mcpServers"]["other"] = serde_json::json!({"command": "other"});
                v
            })
            .unwrap(),
        )
        .unwrap();

        let removed = remove_json_deep_merge(&target, fragment).unwrap();
        assert!(removed);

        let v = read_json(&target);
        assert!(v["mcpServers"]["infigraph"].is_null());
        assert_eq!(v["mcpServers"]["other"]["command"], "other");
    }

    #[test]
    fn remove_json_deep_merge_returns_false_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mcp.json");
        std::fs::write(&target, r#"{"mcpServers":{}}"#).unwrap();
        let removed =
            remove_json_deep_merge(&target, r#"{"mcpServers":{"infigraph":{}}}"#).unwrap();
        assert!(!removed);
    }

    #[test]
    fn remove_overwrite_deletes_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("infigraph.mdc");
        std::fs::write(&target, "content").unwrap();
        let removed = remove_overwrite(&target).unwrap();
        assert!(removed);
        assert!(!target.exists());
    }

    #[test]
    fn remove_overwrite_returns_false_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("does-not-exist.mdc");
        let removed = remove_overwrite(&target).unwrap();
        assert!(!removed);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture`
Expected: compile errors (none of the types/functions exist yet).

- [ ] **Step 3: Implement `merge_json`, `apply_json_deep_merge`, `apply_overwrite`, and their removal counterparts**

Prepend to `crates/infigraph-cli/src/artifacts/strategy.rs` (above the `#[cfg(test)]` block):

```rust
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ApplyOutcome {
    Written,
    Skipped {
        reason: String,
        manual_snippet: String,
    },
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

/// An existing array entry is "ours" if either its serialized form contains
/// the substring "infigraph" (self-heals path-bearing entries like hook
/// commands whose matcher/command drifted), or it exactly equals one of the
/// fragment's own entries (avoids duplicating scalar-ish owned values, e.g.
/// an MCP entry's `args: ["--mcp"]`, which never contains that substring).
fn is_owned_array_entry(entry: &serde_json::Value, fragment_entries: &[serde_json::Value]) -> bool {
    entry.to_string().contains("infigraph") || fragment_entries.contains(entry)
}

pub(crate) fn merge_json(target: &mut serde_json::Value, fragment: &serde_json::Value) {
    match fragment {
        serde_json::Value::Object(fragment_map) => {
            if !target.is_object() {
                *target = serde_json::Value::Object(serde_json::Map::new());
            }
            let target_map = target.as_object_mut().expect("just ensured object");
            for (key, frag_value) in fragment_map {
                let entry = target_map
                    .entry(key.clone())
                    .or_insert(serde_json::Value::Null);
                merge_json(entry, frag_value);
            }
        }
        serde_json::Value::Array(fragment_entries) => {
            let mut merged: Vec<serde_json::Value> = target
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| !is_owned_array_entry(entry, fragment_entries))
                .collect();
            merged.extend(fragment_entries.clone());
            *target = serde_json::Value::Array(merged);
        }
        other => {
            *target = other.clone();
        }
    }
}

/// Mirrors `merge_json`'s traversal to compute what to remove: recurse into
/// object keys the fragment mentions; when a fragment key's value is a plain
/// scalar, remove that key outright; when it's an array, strip owned entries;
/// when it's an object, recurse, then remove the key itself if the recursion
/// left it empty (so e.g. `mcpServers.infigraph` disappears entirely rather
/// than being left behind as `{}`).
fn remove_json_keys(target: &mut serde_json::Value, fragment: &serde_json::Value) -> bool {
    let serde_json::Value::Object(fragment_map) = fragment else {
        return false;
    };
    let Some(target_map) = target.as_object_mut() else {
        return false;
    };

    let mut removed_any = false;
    for (key, frag_value) in fragment_map {
        match frag_value {
            serde_json::Value::Object(_) => {
                if let Some(child) = target_map.get_mut(key) {
                    let child_changed = remove_json_keys(child, frag_value);
                    removed_any |= child_changed;
                    let child_is_empty = child
                        .as_object()
                        .map(|m| m.is_empty())
                        .unwrap_or(false);
                    if child_changed && child_is_empty {
                        target_map.remove(key);
                    }
                }
            }
            serde_json::Value::Array(fragment_entries) => {
                if let Some(existing) = target_map.get_mut(key).and_then(|v| v.as_array_mut()) {
                    let before = existing.len();
                    existing.retain(|entry| !is_owned_array_entry(entry, fragment_entries));
                    removed_any |= existing.len() < before;
                }
            }
            _ => {
                if target_map.remove(key).is_some() {
                    removed_any = true;
                }
            }
        }
    }
    removed_any
}

pub(crate) fn apply_json_deep_merge(target_path: &Path, fragment_content: &str) -> Result<ApplyOutcome> {
    let fragment: serde_json::Value = serde_json::from_str(fragment_content)
        .context("bundled/user fragment is not valid JSON (this is an infigraph bug, please report)")?;

    let mut target: serde_json::Value = if target_path.is_file() {
        let raw = std::fs::read_to_string(target_path)
            .with_context(|| format!("failed to read {}", target_path.display()))?;
        match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ApplyOutcome::Skipped {
                    reason: format!(
                        "{} is not valid JSON ({e}) -- possibly hand-edited with comments or trailing commas",
                        target_path.display()
                    ),
                    manual_snippet: fragment_content.to_string(),
                });
            }
        }
    } else {
        serde_json::json!({})
    };

    merge_json(&mut target, &fragment);

    ensure_parent_dir(target_path)?;
    let pretty = serde_json::to_string_pretty(&target)?;
    std::fs::write(target_path, pretty)
        .with_context(|| format!("failed to write {}", target_path.display()))?;
    Ok(ApplyOutcome::Written)
}

pub(crate) fn apply_overwrite(target_path: &Path, content: &[u8]) -> Result<ApplyOutcome> {
    ensure_parent_dir(target_path)?;
    std::fs::write(target_path, content)
        .with_context(|| format!("failed to write {}", target_path.display()))?;
    Ok(ApplyOutcome::Written)
}

pub(crate) fn remove_json_deep_merge(target_path: &Path, fragment_content: &str) -> Result<bool> {
    if !target_path.is_file() {
        return Ok(false);
    }
    let fragment: serde_json::Value = serde_json::from_str(fragment_content)
        .context("bundled/user fragment is not valid JSON (this is an infigraph bug, please report)")?;
    let raw = std::fs::read_to_string(target_path)
        .with_context(|| format!("failed to read {}", target_path.display()))?;
    let mut target: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    let removed = remove_json_keys(&mut target, &fragment);
    if removed {
        let pretty = serde_json::to_string_pretty(&target)?;
        std::fs::write(target_path, pretty)
            .with_context(|| format!("failed to write {}", target_path.display()))?;
    }
    Ok(removed)
}

pub(crate) fn remove_overwrite(target_path: &Path) -> Result<bool> {
    if !target_path.is_file() {
        return Ok(false);
    }
    std::fs::remove_file(target_path)
        .with_context(|| format!("failed to remove {}", target_path.display()))?;
    Ok(true)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture`
Expected: all 13 tests pass, including `json_deep_merge_reapply_does_not_duplicate_scalar_owned_array`.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/strategy.rs
git commit -m "feat(cli): json_deep_merge and overwrite artifact strategies"
```

---

## Task 5: `marker_delimited` strategy

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/strategy.rs`

**Interfaces:**
- Consumes: `ApplyOutcome`, `ensure_parent_dir` (private helper) from Task 4.
- Produces:
  - `pub(crate) fn apply_marker_delimited(target_path: &std::path::Path, start: &str, end: &str, content: &str) -> anyhow::Result<ApplyOutcome>`
  - `pub(crate) fn remove_marker_delimited(target_path: &std::path::Path, start: &str, end: &str) -> anyhow::Result<bool>`

- [ ] **Step 1: Write the failing tests**

Append inside the existing `#[cfg(test)] mod tests { ... }` block in `crates/infigraph-cli/src/artifacts/strategy.rs` (before the closing `}`):

```rust
    #[test]
    fn marker_delimited_inserts_into_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        apply_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
            "## Infigraph instructions",
        )
        .unwrap();
        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("<!-- infigraph-primary-search -->"));
        assert!(content.contains("## Infigraph instructions"));
        assert!(content.contains("<!-- /infigraph-primary-search -->"));
    }

    #[test]
    fn marker_delimited_preserves_content_outside_markers() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        std::fs::write(&target, "# My project notes\n\nSome custom content.\n").unwrap();

        apply_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
            "## Infigraph instructions",
        )
        .unwrap();

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("# My project notes"));
        assert!(content.contains("Some custom content."));
        assert!(content.contains("## Infigraph instructions"));
    }

    #[test]
    fn marker_delimited_reapply_replaces_not_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");

        apply_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
            "old instructions",
        )
        .unwrap();
        apply_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
            "new instructions",
        )
        .unwrap();

        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            content.matches("<!-- infigraph-primary-search -->").count(),
            1,
            "marker should appear exactly once, got: {content}"
        );
        assert!(!content.contains("old instructions"));
        assert!(content.contains("new instructions"));
    }

    #[test]
    fn remove_marker_delimited_strips_block_and_keeps_rest() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        std::fs::write(&target, "# My notes\n").unwrap();
        apply_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
            "instructions",
        )
        .unwrap();

        let removed = remove_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
        )
        .unwrap();
        assert!(removed);

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("# My notes"));
        assert!(!content.contains("infigraph-primary-search"));
        assert!(!content.contains("instructions"));
    }

    #[test]
    fn remove_marker_delimited_returns_false_when_marker_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        std::fs::write(&target, "# My notes\n").unwrap();
        let removed = remove_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
        )
        .unwrap();
        assert!(!removed);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture`
Expected: compile errors for the 5 new tests (`apply_marker_delimited`/`remove_marker_delimited` don't exist yet); the 13 tests from Task 4 still pass.

- [ ] **Step 3: Implement `apply_marker_delimited` and `remove_marker_delimited`**

Add to `crates/infigraph-cli/src/artifacts/strategy.rs`, just above the `#[cfg(test)]` block:

```rust
pub(crate) fn apply_marker_delimited(
    target_path: &Path,
    start: &str,
    end: &str,
    content: &str,
) -> Result<ApplyOutcome> {
    let existing = if target_path.is_file() {
        std::fs::read_to_string(target_path)
            .with_context(|| format!("failed to read {}", target_path.display()))?
    } else {
        String::new()
    };

    let block = format!("{start}\n{content}\n{end}");

    let new_content = if let Some(start_pos) = existing.find(start) {
        let end_pos = existing[start_pos..]
            .find(end)
            .map(|p| start_pos + p + end.len())
            .unwrap_or(existing.len());
        format!("{}{}{}", &existing[..start_pos], block, &existing[end_pos..])
    } else {
        let sep = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        format!("{existing}{sep}{block}\n")
    };

    ensure_parent_dir(target_path)?;
    std::fs::write(target_path, new_content)
        .with_context(|| format!("failed to write {}", target_path.display()))?;
    Ok(ApplyOutcome::Written)
}

pub(crate) fn remove_marker_delimited(target_path: &Path, start: &str, end: &str) -> Result<bool> {
    if !target_path.is_file() {
        return Ok(false);
    }
    let existing = std::fs::read_to_string(target_path)
        .with_context(|| format!("failed to read {}", target_path.display()))?;
    let Some(start_pos) = existing.find(start) else {
        return Ok(false);
    };
    let end_pos = existing[start_pos..]
        .find(end)
        .map(|p| start_pos + p + end.len())
        .unwrap_or(existing.len());

    let removed = format!(
        "{}{}",
        existing[..start_pos].trim_end(),
        &existing[end_pos..]
    );
    let trimmed = removed.trim_end();
    let final_content = if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    };
    std::fs::write(target_path, final_content)
        .with_context(|| format!("failed to write {}", target_path.display()))?;
    Ok(true)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture`
Expected: all 18 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/strategy.rs
git commit -m "feat(cli): marker_delimited artifact strategy"
```

---

## Task 6: `toml_section` strategy

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/strategy.rs`

**Interfaces:**
- Consumes: `ApplyOutcome`, `ensure_parent_dir` from Task 4.
- Produces:
  - `pub(crate) fn apply_toml_section(target_path: &std::path::Path, key_path: &[String], body: &str) -> anyhow::Result<ApplyOutcome>`
  - `pub(crate) fn remove_toml_section(target_path: &std::path::Path, key_path: &[String]) -> anyhow::Result<bool>`

This directly generalizes the existing `install_toml_target`/`uninstall_toml_target` logic in `config_targets.rs:112-147,184-227` to an arbitrary `key_path` instead of the hardcoded `[mcp_servers.infigraph]` string — Plan 2 will delete those functions once this replaces them.

- [ ] **Step 1: Write the failing tests**

Append inside the `#[cfg(test)] mod tests { ... }` block in `crates/infigraph-cli/src/artifacts/strategy.rs`:

```rust
    #[test]
    fn toml_section_writes_into_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        apply_toml_section(
            &target,
            &["mcp_servers".to_string(), "infigraph".to_string()],
            "command = \"/bin/infigraph-mcp\"\nargs = [\"--mcp\"]",
        )
        .unwrap();

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("[mcp_servers.infigraph]"));
        assert!(content.contains("command = \"/bin/infigraph-mcp\""));
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["infigraph"]["command"].as_str(),
            Some("/bin/infigraph-mcp")
        );
    }

    #[test]
    fn toml_section_preserves_unrelated_sections_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        std::fs::write(
            &target,
            "# a user comment\n[mcp_servers.other]\ncommand = \"other\"\n",
        )
        .unwrap();

        apply_toml_section(
            &target,
            &["mcp_servers".to_string(), "infigraph".to_string()],
            "command = \"/bin/infigraph-mcp\"\nargs = [\"--mcp\"]",
        )
        .unwrap();

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("# a user comment"));
        assert!(content.contains("[mcp_servers.other]"));
        assert!(content.contains("command = \"other\""));
        assert!(content.contains("[mcp_servers.infigraph]"));
    }

    #[test]
    fn toml_section_reapply_replaces_not_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        let key_path = vec!["mcp_servers".to_string(), "infigraph".to_string()];

        apply_toml_section(&target, &key_path, "command = \"/old/path\"").unwrap();
        apply_toml_section(&target, &key_path, "command = \"/new/path\"").unwrap();

        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            content.matches("[mcp_servers.infigraph]").count(),
            1,
            "section header must appear exactly once, got: {content}"
        );
        assert!(!content.contains("/old/path"));
        assert!(content.contains("/new/path"));
    }

    #[test]
    fn remove_toml_section_strips_only_named_section() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        std::fs::write(&target, "[mcp_servers.other]\ncommand = \"other\"\n").unwrap();
        let key_path = vec!["mcp_servers".to_string(), "infigraph".to_string()];
        apply_toml_section(&target, &key_path, "command = \"/bin/infigraph-mcp\"").unwrap();

        let removed = remove_toml_section(&target, &key_path).unwrap();
        assert!(removed);

        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.contains("[mcp_servers.other]"));
        assert!(!content.contains("[mcp_servers.infigraph]"));
        assert!(!content.contains("infigraph-mcp"));
    }

    #[test]
    fn remove_toml_section_returns_false_when_section_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.toml");
        std::fs::write(&target, "[mcp_servers.other]\ncommand = \"other\"\n").unwrap();
        let removed = remove_toml_section(
            &target,
            &["mcp_servers".to_string(), "infigraph".to_string()],
        )
        .unwrap();
        assert!(!removed);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture`
Expected: compile errors for the 5 new tests; prior 18 tests still pass.

- [ ] **Step 3: Implement `apply_toml_section` and `remove_toml_section`**

Add to `crates/infigraph-cli/src/artifacts/strategy.rs`, just above `#[cfg(test)]`:

```rust
fn toml_section_header(key_path: &[String]) -> String {
    format!("[{}]", key_path.join("."))
}

pub(crate) fn apply_toml_section(target_path: &Path, key_path: &[String], body: &str) -> Result<ApplyOutcome> {
    anyhow::ensure!(!key_path.is_empty(), "toml_section requires a non-empty key_path");
    let header = toml_section_header(key_path);

    let existing = if target_path.is_file() {
        std::fs::read_to_string(target_path)
            .with_context(|| format!("failed to read {}", target_path.display()))?
    } else {
        String::new()
    };

    let section = format!("{header}\n{}\n", body.trim_end());

    let new_content = if let Some(start) = existing.find(&header) {
        let after_header = start + header.len();
        let section_end = existing[after_header..]
            .find("\n[")
            .map(|pos| after_header + pos + 1)
            .unwrap_or(existing.len());
        format!("{}{}{}", &existing[..start], section, &existing[section_end..])
    } else if existing.is_empty() {
        section
    } else {
        let sep = if existing.ends_with('\n') { "" } else { "\n" };
        format!("{existing}{sep}\n{section}")
    };

    ensure_parent_dir(target_path)?;
    std::fs::write(target_path, new_content)
        .with_context(|| format!("failed to write {}", target_path.display()))?;
    Ok(ApplyOutcome::Written)
}

pub(crate) fn remove_toml_section(target_path: &Path, key_path: &[String]) -> Result<bool> {
    if !target_path.is_file() {
        return Ok(false);
    }
    let header = toml_section_header(key_path);
    let content = std::fs::read_to_string(target_path)
        .with_context(|| format!("failed to read {}", target_path.display()))?;

    let Some(start) = content.find(&header) else {
        return Ok(false);
    };
    let after_header = start + header.len();
    let section_end = content[after_header..]
        .find("\n[")
        .map(|pos| after_header + pos + 1)
        .unwrap_or(content.len());

    let new_content = format!("{}{}", &content[..start], &content[section_end..]);
    let trimmed = new_content.trim_end();
    let final_content = if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    };
    std::fs::write(target_path, final_content)
        .with_context(|| format!("failed to write {}", target_path.display()))?;
    Ok(true)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture`
Expected: all 23 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/strategy.rs
git commit -m "feat(cli): toml_section artifact strategy"
```

---

## Task 7: Convention-based classification (extension + path → strategy)

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/convention.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks (pure path/string logic).
- Produces: `pub(crate) fn infer_strategy(relative_path: &str) -> Option<ConventionStrategy>` and `pub(crate) enum ConventionStrategy { JsonDeepMerge, Overwrite }`

- [ ] **Step 1: Write the failing tests**

Write `crates/infigraph-cli/src/artifacts/convention.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_extension_infers_deep_merge() {
        assert_eq!(
            infer_strategy("claude-code/.claude.json"),
            Some(ConventionStrategy::JsonDeepMerge)
        );
        assert_eq!(
            infer_strategy("gemini-cli/.gemini/settings.json"),
            Some(ConventionStrategy::JsonDeepMerge)
        );
    }

    #[test]
    fn toml_extension_is_never_convention_based() {
        assert_eq!(infer_strategy("codex/.codex/config.toml"), None);
        assert_eq!(infer_strategy("codex/mcp-section.toml"), None);
    }

    #[test]
    fn shell_and_markdown_infer_overwrite() {
        assert_eq!(
            infer_strategy("claude-code/hooks/enforce.sh"),
            Some(ConventionStrategy::Overwrite)
        );
        assert_eq!(
            infer_strategy("shared/skills/infigraph-reindex/SKILL.md"),
            Some(ConventionStrategy::Overwrite)
        );
        assert_eq!(
            infer_strategy("cursor/rules/infigraph.mdc"),
            Some(ConventionStrategy::Overwrite)
        );
    }

    #[test]
    fn manifest_filename_is_not_a_convention_artifact() {
        // config.toml is the manifest itself -- discovery (Task 9) must never
        // treat it as convention-based content, but infer_strategy alone
        // (extension-only) can't distinguish this; .toml already returns None
        // unconditionally, which is sufficient for that guarantee here.
        assert_eq!(infer_strategy("claude-code/config.toml"), None);
    }

    #[test]
    fn extensionless_and_unknown_extensions_return_none() {
        assert_eq!(infer_strategy("claude-code/hooks/no-extension"), None);
        assert_eq!(infer_strategy("some/path/file.exe"), Some(ConventionStrategy::Overwrite));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::convention -- --nocapture`
Expected: compile error (`infer_strategy`, `ConventionStrategy` don't exist yet).

- [ ] **Step 3: Implement `infer_strategy`**

Prepend to `crates/infigraph-cli/src/artifacts/convention.rs` (above `#[cfg(test)]`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConventionStrategy {
    JsonDeepMerge,
    Overwrite,
}

/// Infers the strategy for a bundled/user-override file that has no explicit
/// `[[artifact]]` manifest entry, from its extension alone. `.toml` files are
/// deliberately excluded (return `None`) -- they always need a manifest
/// entry declaring `strategy = "toml_section"` and a `key_path`, since a
/// surgical raw-text section splice can't be inferred from extension+path
/// the way JSON's structural merge can (see the design spec's "core idea",
/// reason 3). Any other recognized extension without special merge
/// semantics (including no extension is not "recognized" -- see below) maps
/// to a plain `overwrite`.
pub(crate) fn infer_strategy(relative_path: &str) -> Option<ConventionStrategy> {
    let extension = std::path::Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())?;

    match extension {
        "json" => Some(ConventionStrategy::JsonDeepMerge),
        "toml" => None,
        _ => Some(ConventionStrategy::Overwrite),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::convention -- --nocapture`
Expected: all 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/convention.rs
git commit -m "feat(cli): convention-based strategy inference from file extension"
```

---

## Task 8: Resolver subprocess IPC + `json_key_path` strategy

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/resolver.rs`
- Modify: `crates/infigraph-cli/src/artifacts/strategy.rs`

**Interfaces:**
- Consumes: `ApplyOutcome`, `ensure_parent_dir` from Task 4.
- Produces:
  - `pub(crate) struct ResolverInput { pub mcp_path: String, pub os: String, pub home: String }` (with `Serialize`)
  - `pub(crate) enum ResolverOutput { Ok { data: ResolverData }, Skip { message: String }, Error { message: String } }`
  - `pub(crate) struct ResolverData { pub path: String, pub content: Option<serde_json::Value> }`
  - `pub(crate) fn run_resolver(resolver_cmd: &[String], cwd: &std::path::Path, mcp_path: &str, home: &std::path::Path) -> anyhow::Result<ResolverOutput>`
  - `pub(crate) fn apply_json_key_path(target_path: &std::path::Path, key_path: &[String], value_content: &str) -> anyhow::Result<ApplyOutcome>`
  - `pub(crate) fn remove_json_key_path(target_path: &std::path::Path, key_path: &[String]) -> anyhow::Result<bool>`

- [ ] **Step 1: Write the failing tests for the resolver IPC**

Write `crates/infigraph-cli/src/artifacts/resolver.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    fn write_fake_resolver(dir: &std::path::Path, name: &str, script: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn resolver_ok_response_with_content() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"ok\",\"data\":{\"path\":\"/resolved/path.json\",\"content\":{\"command\":\"/bin/infigraph-mcp\"}}}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => {
                assert_eq!(data.path, "/resolved/path.json");
                assert_eq!(data.content.unwrap()["command"], "/bin/infigraph-mcp");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn resolver_ok_response_without_content() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"ok\",\"data\":{\"path\":\"/resolved/path.json\"}}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => {
                assert_eq!(data.path, "/resolved/path.json");
                assert!(data.content.is_none());
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn resolver_skip_response() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"skip\",\"message\":\"not installed\"}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Skip { message } => assert_eq!(message, "not installed"),
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn resolver_error_response() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"error\",\"message\":\"could not detect profile\"}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Error { message } => assert_eq!(message, "could not detect profile"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn resolver_receives_correct_stdin_shape() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "echo_input.sh",
            "#!/usr/bin/env bash\ninput=$(cat)\necho \"{\\\"status\\\":\\\"ok\\\",\\\"data\\\":{\\\"path\\\":\\\"$input\\\"}}\"\n",
        );

        let output = run_resolver(
            &["./echo_input.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => {
                assert!(data.path.contains("\"mcp_path\":\"/bin/infigraph-mcp\""));
                assert!(data.path.contains("\"home\":\"/home/x\""));
                assert!(data.path.contains("\"os\":"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::resolver -- --nocapture`
Expected: compile error (`run_resolver`, `ResolverOutput`, etc. don't exist yet).

- [ ] **Step 3: Implement the resolver IPC**

Prepend to `crates/infigraph-cli/src/artifacts/resolver.rs` (above `#[cfg(test)]`):

```rust
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResolverInput {
    pub mcp_path: String,
    pub os: String,
    pub home: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResolverData {
    pub path: String,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum ResolverOutput {
    Ok { data: ResolverData },
    Skip { message: String },
    Error { message: String },
}

/// Runs a resolver executable (`resolver_cmd[0]` resolved relative to `cwd`,
/// with `resolver_cmd[1..]` as its arguments), feeding it the standard
/// resolver-contract JSON on stdin and parsing its JSON stdout response.
pub(crate) fn run_resolver(
    resolver_cmd: &[String],
    cwd: &Path,
    mcp_path: &str,
    home: &Path,
) -> Result<ResolverOutput> {
    anyhow::ensure!(!resolver_cmd.is_empty(), "resolver command must not be empty");

    let input = ResolverInput {
        mcp_path: mcp_path.to_string(),
        os: std::env::consts::OS.to_string(),
        home: home.to_string_lossy().to_string(),
    };
    let input_json = serde_json::to_string(&input)?;

    let mut command = std::process::Command::new(&resolver_cmd[0]);
    command
        .args(&resolver_cmd[1..])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn resolver {}", resolver_cmd[0]))?;

    {
        use std::io::Write as _;
        let stdin = child.stdin.as_mut().expect("stdin was piped");
        stdin
            .write_all(input_json.as_bytes())
            .context("failed to write resolver input")?;
    }

    let output = child
        .wait_with_output()
        .context("failed to wait for resolver process")?;

    anyhow::ensure!(
        output.status.success(),
        "resolver {} exited with {:?}: {}",
        resolver_cmd[0],
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout)
        .with_context(|| format!("resolver {} returned invalid JSON: {stdout}", resolver_cmd[0]))
}
```

- [ ] **Step 4: Run the resolver tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::resolver -- --nocapture`
Expected: all 5 tests pass.

- [ ] **Step 5: Write the failing tests for `json_key_path`**

Append inside the `#[cfg(test)] mod tests { ... }` block in `crates/infigraph-cli/src/artifacts/strategy.rs`:

```rust
    #[test]
    fn json_key_path_sets_nested_value_in_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        apply_json_key_path(
            &target,
            &["context_servers".to_string(), "infigraph".to_string()],
            r#"{"command":"/bin/infigraph-mcp","args":["--mcp"],"env":{}}"#,
        )
        .unwrap();

        let v = read_json(&target);
        assert_eq!(v["context_servers"]["infigraph"]["command"], "/bin/infigraph-mcp");
    }

    #[test]
    fn json_key_path_preserves_unrelated_top_level_keys() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(&target, r#"{"theme":"dark","font_size":14}"#).unwrap();

        apply_json_key_path(
            &target,
            &["context_servers".to_string(), "infigraph".to_string()],
            r#"{"command":"/bin/infigraph-mcp"}"#,
        )
        .unwrap();

        let v = read_json(&target);
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["font_size"], 14);
        assert_eq!(v["context_servers"]["infigraph"]["command"], "/bin/infigraph-mcp");
    }

    #[test]
    fn json_key_path_reapply_replaces_not_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        let key_path = vec!["context_servers".to_string(), "infigraph".to_string()];

        apply_json_key_path(&target, &key_path, r#"{"command":"/old"}"#).unwrap();
        apply_json_key_path(&target, &key_path, r#"{"command":"/new"}"#).unwrap();

        let v = read_json(&target);
        assert_eq!(v["context_servers"]["infigraph"]["command"], "/new");
        assert!(
            v["context_servers"].as_object().unwrap().len() == 1,
            "should not accumulate duplicate sibling keys"
        );
    }

    #[test]
    fn json_key_path_parse_failure_returns_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(&target, "{ trailing, comma, }").unwrap();
        let before = std::fs::read_to_string(&target).unwrap();

        let outcome = apply_json_key_path(
            &target,
            &["context_servers".to_string(), "infigraph".to_string()],
            r#"{"command":"/bin/infigraph-mcp"}"#,
        )
        .unwrap();
        assert!(matches!(outcome, ApplyOutcome::Skipped { .. }));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), before);
    }

    #[test]
    fn remove_json_key_path_removes_leaf_and_keeps_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        std::fs::write(&target, r#"{"context_servers":{"other":{"command":"other"}}}"#).unwrap();
        let key_path = vec!["context_servers".to_string(), "infigraph".to_string()];
        apply_json_key_path(&target, &key_path, r#"{"command":"/bin/infigraph-mcp"}"#).unwrap();

        let removed = remove_json_key_path(&target, &key_path).unwrap();
        assert!(removed);

        let v = read_json(&target);
        assert!(v["context_servers"]["infigraph"].is_null());
        assert_eq!(v["context_servers"]["other"]["command"], "other");
    }
```

- [ ] **Step 6: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture`
Expected: compile errors for the 5 new tests; prior 23 tests still pass.

- [ ] **Step 7: Implement `apply_json_key_path` and `remove_json_key_path`**

Add to `crates/infigraph-cli/src/artifacts/strategy.rs`, just above `#[cfg(test)]`:

```rust
fn navigate_to_parent<'a>(
    root: &'a mut serde_json::Value,
    key_path: &[String],
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    if !root.is_object() {
        *root = serde_json::json!({});
    }
    let mut cursor = root;
    for key in &key_path[..key_path.len() - 1] {
        if !cursor.is_object() {
            *cursor = serde_json::json!({});
        }
        cursor = cursor
            .as_object_mut()
            .expect("just ensured object")
            .entry(key.clone())
            .or_insert_with(|| serde_json::json!({}));
    }
    if !cursor.is_object() {
        *cursor = serde_json::json!({});
    }
    cursor.as_object_mut().expect("just ensured object")
}

pub(crate) fn apply_json_key_path(
    target_path: &Path,
    key_path: &[String],
    value_content: &str,
) -> Result<ApplyOutcome> {
    anyhow::ensure!(!key_path.is_empty(), "json_key_path requires a non-empty key_path");
    let value: serde_json::Value = serde_json::from_str(value_content)
        .context("resolver/content value is not valid JSON (this is an infigraph bug, please report)")?;

    let mut target: serde_json::Value = if target_path.is_file() {
        let raw = std::fs::read_to_string(target_path)
            .with_context(|| format!("failed to read {}", target_path.display()))?;
        match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ApplyOutcome::Skipped {
                    reason: format!(
                        "{} is not valid JSON ({e}) -- possibly hand-edited with comments or trailing commas",
                        target_path.display()
                    ),
                    manual_snippet: format!("{}: {}", key_path.join("."), value_content),
                });
            }
        }
    } else {
        serde_json::json!({})
    };

    let leaf_key = key_path.last().expect("checked non-empty above").clone();
    navigate_to_parent(&mut target, key_path).insert(leaf_key, value);

    ensure_parent_dir(target_path)?;
    let pretty = serde_json::to_string_pretty(&target)?;
    std::fs::write(target_path, pretty)
        .with_context(|| format!("failed to write {}", target_path.display()))?;
    Ok(ApplyOutcome::Written)
}

pub(crate) fn remove_json_key_path(target_path: &Path, key_path: &[String]) -> Result<bool> {
    anyhow::ensure!(!key_path.is_empty(), "json_key_path requires a non-empty key_path");
    if !target_path.is_file() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(target_path)
        .with_context(|| format!("failed to read {}", target_path.display()))?;
    let mut target: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    let mut cursor = &mut target;
    for key in &key_path[..key_path.len() - 1] {
        let Some(next) = cursor.get_mut(key) else {
            return Ok(false);
        };
        cursor = next;
    }
    let leaf_key = key_path.last().expect("checked non-empty above");
    let Some(map) = cursor.as_object_mut() else {
        return Ok(false);
    };
    let removed = map.remove(leaf_key).is_some();

    if removed {
        let pretty = serde_json::to_string_pretty(&target)?;
        std::fs::write(target_path, pretty)
            .with_context(|| format!("failed to write {}", target_path.display()))?;
    }
    Ok(removed)
}
```

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::strategy -- --nocapture` and `cargo test -p infigraph-cli artifacts::resolver -- --nocapture`
Expected: all 28 `strategy` tests and all 5 `resolver` tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/resolver.rs crates/infigraph-cli/src/artifacts/strategy.rs
git commit -m "feat(cli): resolver subprocess IPC and json_key_path strategy"
```

---

## Task 9: `build.rs` bundled registry + discovery (bundled + user-override merge)

**Files:**
- Create: `crates/infigraph-cli/build.rs`
- Modify: `crates/infigraph-cli/Cargo.toml` (declare `build = "build.rs"`)
- Modify: `crates/infigraph-cli/src/artifacts/discovery.rs`
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (expose `Strategy`, `ResolvedArtifact`, `discover_artifacts` from submodules)

**Interfaces:**
- Consumes: `manifest::{IntegrationManifest, ArtifactEntry, parse_manifest}` (Task 2), `template::{TemplateFormat, substitute_mcp_path}` (Task 3), `convention::{ConventionStrategy, infer_strategy}` (Task 7), `step::InstallStep` (Task 10 — this task only references the type name; Task 10 defines it, dispatched in the reverse task order note below).
- Produces:
  - `pub(crate) enum Strategy { JsonDeepMerge, Overwrite, MarkerDelimited, TomlSection, JsonKeyPath }`
  - `pub(crate) struct ResolvedArtifact { pub integration_label: String, pub target_relative_path: String, pub strategy: Strategy, pub content: Option<Vec<u8>>, pub start: Option<String>, pub end: Option<String>, pub key_path: Option<Vec<String>>, pub resolver: Option<(Vec<String>, std::path::PathBuf)> }` — **Task 12 later changes `target_relative_path` to `Option<String>` and `resolver` to `Option<ResolverSpec>`**, once it becomes clear a resolver-driven artifact has no static path and the raw `(Vec<String>, PathBuf)` pair isn't enough to actually spawn a bundled (in-memory) resolver script.
  - `pub(crate) fn discover_artifacts(bundled: &[(&'static str, &'static [u8])], user_override_dir: &std::path::Path, mcp_path: &str) -> anyhow::Result<Vec<ResolvedArtifact>>`

Note on task ordering: this task references `InstallStep` by name only in a doc comment on `ResolvedArtifact` — it does **not** add an `InstallStep` field yet (Task 10 adds that field once the enum exists), to keep each task's diff compiling independently. Task 10 will add `pub step: InstallStep` to `ResolvedArtifact` and update every constructor site.

- [ ] **Step 1: Write the failing tests for `build.rs`'s generated registry shape**

`build.rs` isn't directly unit-testable (it's a separate compilation), so this step instead writes the failing test for `discover_artifacts`, which is what actually consumes the registry shape `build.rs` produces. Write `crates/infigraph-cli/src/artifacts/discovery.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &std::path::Path, relative: &str, content: &str) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn discovers_convention_based_json_artifact_from_bundled() {
        let bundled: &[(&str, &[u8])] = &[(
            "claude-code/.claude.json",
            br#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}"#,
        )];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        let a = &artifacts[0];
        assert_eq!(
            a.target_relative_path, ".claude.json",
            "the leading \"claude-code/\" integration-directory segment must be stripped -- \
             the bundled subtree already mirrors the real path below $HOME"
        );
        assert_eq!(a.strategy, Strategy::JsonDeepMerge);
        let content = String::from_utf8(a.content.clone().unwrap()).unwrap();
        assert!(content.contains("/bin/infigraph-mcp"), "mcp_path should be substituted: {content}");
        assert!(!content.contains("{{mcp_path}}"));
    }

    #[test]
    fn skips_bundled_toml_file_with_no_manifest_entry() {
        // A .toml content file with no config.toml manifest claiming it is a
        // discovery bug waiting to happen (it would silently never be
        // applied) -- but per Global Constraints, .toml is never
        // convention-based, so it's correctly excluded, not silently wrong.
        let bundled: &[(&str, &[u8])] = &[("codex/mcp-section.toml", b"command = \"{{mcp_path}}\"")];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();
        assert!(
            artifacts.is_empty(),
            "a .toml file with no manifest entry referencing it must not be auto-applied"
        );
    }

    #[test]
    fn manifest_entry_produces_explicit_artifact_with_all_fields() {
        let bundled: &[(&str, &[u8])] = &[
            (
                "codex/config.toml",
                br#"label = "Codex"

[[artifact]]
path = ".codex/config.toml"
strategy = "toml_section"
key_path = ["mcp_servers", "infigraph"]
content_file = "mcp-section.toml"
"#,
            ),
            ("codex/mcp-section.toml", b"command = \"{{mcp_path}}\"\nargs = [\"--mcp\"]"),
        ];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        let a = &artifacts[0];
        assert_eq!(a.integration_label, "Codex");
        assert_eq!(a.target_relative_path, ".codex/config.toml");
        assert_eq!(a.strategy, Strategy::TomlSection);
        assert_eq!(
            a.key_path,
            Some(vec!["mcp_servers".to_string(), "infigraph".to_string()])
        );
        let content = String::from_utf8(a.content.clone().unwrap()).unwrap();
        assert!(content.contains("/bin/infigraph-mcp"));
    }

    #[test]
    fn manifest_content_file_can_escape_to_shared_directory() {
        let bundled: &[(&str, &[u8])] = &[
            (
                "claude-code/config.toml",
                br#"label = "Claude Code"

[[artifact]]
path = ".claude/CLAUDE.md"
strategy = "marker_delimited"
start = "<!-- infigraph-primary-search -->"
end = "<!-- /infigraph-primary-search -->"
content_file = "../shared/agents.md"
"#,
            ),
            ("shared/agents.md", b"## Infigraph instructions"),
        ];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            String::from_utf8(artifacts[0].content.clone().unwrap()).unwrap(),
            "## Infigraph instructions"
        );
    }

    #[test]
    fn user_override_replaces_bundled_file_at_same_relative_path() {
        let bundled: &[(&str, &[u8])] = &[("claude-code/hooks/enforce.sh", b"#!/bin/bash\necho bundled\n")];
        let user_dir = tempfile::tempdir().unwrap();
        write_file(user_dir.path(), "claude-code/hooks/enforce.sh", "#!/bin/bash\necho overridden\n");

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            String::from_utf8(artifacts[0].content.clone().unwrap()).unwrap(),
            "#!/bin/bash\necho overridden\n"
        );
    }

    #[test]
    fn user_override_adds_a_new_convention_based_file_with_zero_manifest_changes() {
        let bundled: &[(&str, &[u8])] = &[("claude-code/hooks/enforce.sh", b"bundled")];
        let user_dir = tempfile::tempdir().unwrap();
        write_file(user_dir.path(), "claude-code/hooks/new-hook.sh", "new content");

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 2);
        assert!(artifacts
            .iter()
            .any(|a| a.target_relative_path == "hooks/new-hook.sh"));
    }

    #[test]
    fn user_override_adds_a_wholly_new_integration_directory() {
        let bundled: &[(&str, &[u8])] = &[];
        let user_dir = tempfile::tempdir().unwrap();
        write_file(
            user_dir.path(),
            "my-custom-agent/.custom/mcp.json",
            r#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}"}}}"#,
        );

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].target_relative_path, ".custom/mcp.json");
    }

    #[test]
    fn manifest_at_nested_path_is_not_treated_as_a_manifest() {
        // Only <integration>/config.toml at the integration's own root is a
        // manifest; a same-named file nested under a subdirectory is content.
        let bundled: &[(&str, &[u8])] = &[(
            "codex/some-subdir/config.toml",
            b"not-a-manifest = true",
        )];
        let user_dir = tempfile::tempdir().unwrap();

        // .toml is never convention-based, and this nested file has no
        // [[artifact]] entry referencing it (there's no codex/config.toml
        // manifest at all in this fixture), so it must simply be excluded --
        // not misinterpreted as a manifest and not silently applied.
        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();
        assert!(artifacts.is_empty());
    }

    #[test]
    fn shared_directory_is_not_directly_discovered_as_a_convention_artifact() {
        let bundled: &[(&str, &[u8])] = &[("shared/agents.md", b"shared content, never applied on its own")];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();
        assert!(
            artifacts.is_empty(),
            "shared/ content must only be reachable via a content_file reference, never applied directly"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::discovery -- --nocapture`
Expected: compile error (`discover_artifacts`, `Strategy`, `ResolvedArtifact` don't exist yet).

- [ ] **Step 3: Implement `Strategy` and the discovery merge/resolution logic**

First, add the `Strategy` enum to `crates/infigraph-cli/src/artifacts/strategy.rs`, just above the existing `ApplyOutcome` definition:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strategy {
    JsonDeepMerge,
    Overwrite,
    MarkerDelimited,
    TomlSection,
    JsonKeyPath,
}

impl Strategy {
    pub(crate) fn parse(s: &str) -> anyhow::Result<Strategy> {
        match s {
            "json_deep_merge" => Ok(Strategy::JsonDeepMerge),
            "overwrite" => Ok(Strategy::Overwrite),
            "marker_delimited" => Ok(Strategy::MarkerDelimited),
            "toml_section" => Ok(Strategy::TomlSection),
            "json_key_path" => Ok(Strategy::JsonKeyPath),
            other => anyhow::bail!(
                "unknown artifact strategy \"{other}\" (expected one of: json_deep_merge, overwrite, marker_delimited, toml_section, json_key_path)"
            ),
        }
    }
}
```

Now prepend to `crates/infigraph-cli/src/artifacts/discovery.rs` (above `#[cfg(test)]`):

```rust
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::convention::{infer_strategy, ConventionStrategy};
use super::manifest::parse_manifest;
use super::strategy::Strategy;
use super::template::{substitute_mcp_path, TemplateFormat};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedArtifact {
    pub integration_label: String,
    pub target_relative_path: String,
    pub strategy: Strategy,
    /// Fully resolved (template-substituted) content, ready to hand to the
    /// matching `strategy::apply_*` function. `None` only for resolver-driven
    /// artifacts whose content is generated at apply time (Plan 2 territory);
    /// this plan's discovery always populates it when a `content_file` exists.
    pub content: Option<Vec<u8>>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub key_path: Option<Vec<String>>,
    /// `(resolver_command, integration_directory)` -- the directory a
    /// relative resolver command like `./resolve-zed-path.sh` should be
    /// spawned from.
    pub resolver: Option<(Vec<String>, PathBuf)>,
}

fn is_toml_extension(relative_path: &str) -> bool {
    Path::new(relative_path)
        .extension()
        .and_then(|e| e.to_str())
        == Some("toml")
}

/// True for `<integration>/config.toml` (the manifest), false for any deeper
/// nesting like `<integration>/some-subdir/config.toml` (content that merely
/// shares the filename) or a file elsewhere named differently.
fn is_manifest_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    path.file_name().and_then(|f| f.to_str()) == Some("config.toml")
        && path.components().count() == 2 // "<integration>" / "config.toml"
}

fn integration_dir_of(relative_path: &str) -> &str {
    relative_path.split('/').next().unwrap_or(relative_path)
}

/// Strips the leading `<integration_dir>/` segment from a bundled/user-override
/// relative path, leaving the path relative to `$HOME`. Every convention-based
/// file's location below its integration directory is authored to already
/// mirror the real destination structure (e.g. `claude-code/.claude/hooks/x.sh`
/// strips to `.claude/hooks/x.sh`; `claude-code/.claude.json` strips to
/// `.claude.json`, since that file's real destination has no further nesting)
/// -- so a plain prefix strip is always correct, never a special case per integration.
fn strip_integration_prefix(relative_path: &str, integration_dir: &str) -> String {
    relative_path
        .strip_prefix(integration_dir)
        .and_then(|s| s.strip_prefix('/'))
        .unwrap_or(relative_path)
        .to_string()
}

/// Merges the bundled registry with the user-override directory into one
/// `relative_path -> content bytes` map, override winning at the same path.
fn merge_bundled_and_user(
    bundled: &[(&'static str, &'static [u8])],
    user_override_dir: &Path,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files: BTreeMap<String, Vec<u8>> = bundled
        .iter()
        .map(|(path, bytes)| (path.to_string(), bytes.to_vec()))
        .collect();

    if user_override_dir.is_dir() {
        for entry in walk_files(user_override_dir)? {
            let relative = entry
                .strip_prefix(user_override_dir)
                .expect("walked under this root")
                .to_string_lossy()
                .replace('\\', "/");
            let content = std::fs::read(&entry)
                .with_context(|| format!("failed to read {}", entry.display()))?;
            files.insert(relative, content);
        }
    }

    Ok(files)
}

fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn resolve_content_file(
    files: &BTreeMap<String, Vec<u8>>,
    integration_dir: &str,
    content_file: &str,
) -> Result<Vec<u8>> {
    // content_file is relative to the integration's own directory, and may
    // escape it with "../" (e.g. "../shared/agents.md").
    let combined = format!("{integration_dir}/{content_file}");
    let normalized = normalize_relative_path(&combined);
    files
        .get(&normalized)
        .cloned()
        .with_context(|| format!("content_file \"{content_file}\" (resolved to \"{normalized}\") not found"))
}

fn normalize_relative_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn template_format_for(relative_path: &str) -> Option<TemplateFormat> {
    match Path::new(relative_path).extension().and_then(|e| e.to_str()) {
        Some("json") => Some(TemplateFormat::Json),
        Some("toml") => Some(TemplateFormat::Toml),
        _ => None,
    }
}

pub(crate) fn discover_artifacts(
    bundled: &[(&'static str, &'static [u8])],
    user_override_dir: &Path,
    mcp_path: &str,
) -> Result<Vec<ResolvedArtifact>> {
    let files = merge_bundled_and_user(bundled, user_override_dir)?;
    let mut artifacts = Vec::new();
    let mut manifest_claimed: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 1: explicit [[artifact]] entries from every <integration>/config.toml manifest.
    for (relative_path, content) in &files {
        if !is_manifest_path(relative_path) {
            continue;
        }
        let integration_dir = integration_dir_of(relative_path);
        let manifest_text = String::from_utf8(content.clone())
            .with_context(|| format!("{relative_path} is not valid UTF-8"))?;
        let manifest = parse_manifest(&manifest_text)
            .with_context(|| format!("failed to parse manifest {relative_path}"))?;
        let label = manifest.label.clone().unwrap_or_else(|| integration_dir.to_string());

        for entry in &manifest.artifacts {
            let strategy = Strategy::parse(&entry.strategy)
                .with_context(|| format!("in manifest {relative_path}, artifact \"{}\"", entry.path))?;

            let content = match &entry.content_file {
                Some(content_file) => {
                    let raw = resolve_content_file(&files, integration_dir, content_file)?;
                    let normalized_content_file =
                        normalize_relative_path(&format!("{integration_dir}/{content_file}"));
                    manifest_claimed.insert(normalized_content_file);
                    let text = String::from_utf8(raw)
                        .with_context(|| format!("content_file \"{content_file}\" is not valid UTF-8"))?;
                    let substituted = match template_format_for(&entry.path) {
                        Some(format) => substitute_mcp_path(&text, mcp_path, format),
                        None => text,
                    };
                    Some(substituted.into_bytes())
                }
                None => None,
            };

            artifacts.push(ResolvedArtifact {
                integration_label: label.clone(),
                target_relative_path: entry.path.clone(),
                strategy,
                content,
                start: entry.start.clone(),
                end: entry.end.clone(),
                key_path: entry.key_path.clone(),
                resolver: entry
                    .resolver
                    .clone()
                    .map(|cmd| (cmd, PathBuf::from(integration_dir))),
            });
        }
    }

    // Pass 2: convention-based files -- everything not a manifest, not
    // claimed as a manifest's content_file, and not directly under shared/.
    for (relative_path, content) in &files {
        if is_manifest_path(relative_path) {
            continue;
        }
        if manifest_claimed.contains(relative_path) {
            continue;
        }
        if integration_dir_of(relative_path) == "shared" {
            continue;
        }
        if is_toml_extension(relative_path) {
            // .toml is never convention-based; without a manifest entry
            // referencing it, it is simply not applied (proven by the
            // skips_bundled_toml_file_with_no_manifest_entry test above).
            continue;
        }

        let Some(convention) = infer_strategy(relative_path) else {
            continue;
        };
        let strategy = match convention {
            ConventionStrategy::JsonDeepMerge => Strategy::JsonDeepMerge,
            ConventionStrategy::Overwrite => Strategy::Overwrite,
        };

        let text_for_template = match template_format_for(relative_path) {
            Some(format) => {
                let text = String::from_utf8(content.clone())
                    .with_context(|| format!("{relative_path} is not valid UTF-8"))?;
                substitute_mcp_path(&text, mcp_path, format).into_bytes()
            }
            None => content.clone(),
        };

        let integration_dir = integration_dir_of(relative_path);
        // The bundled subtree below <integration>/ already mirrors the real
        // destination path exactly (e.g. "claude-code/.claude/hooks/x.sh" is
        // authored that way specifically so stripping the leading
        // "claude-code/" leaves ".claude/hooks/x.sh", the correct path
        // relative to $HOME) -- except ".claude.json" itself, which sits
        // at the integration root because its real destination has no
        // further nesting. Both cases are handled by the same strip.
        let target_relative_path = strip_integration_prefix(relative_path, integration_dir);
        artifacts.push(ResolvedArtifact {
            integration_label: integration_dir.to_string(),
            target_relative_path,
            strategy,
            content: Some(text_for_template),
            start: None,
            end: None,
            key_path: None,
            resolver: None,
        });
    }

    Ok(artifacts)
}
```

- [ ] **Step 4: Create `build.rs`**

Write `crates/infigraph-cli/build.rs`:

```rust
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo");
    let resources_dir = Path::new(&manifest_dir).join("resources/integrations");
    println!("cargo:rerun-if-changed={}", resources_dir.display());

    let mut entries: Vec<(String, String)> = Vec::new(); // (relative_path, absolute_path)
    if resources_dir.is_dir() {
        collect_files(&resources_dir, &resources_dir, &mut entries);
    }
    entries.sort();

    let mut generated = String::from(
        "pub(crate) static BUNDLED_INTEGRATIONS: &[(&str, &[u8])] = &[\n",
    );
    for (relative, absolute) in &entries {
        generated.push_str(&format!(
            "    ({relative:?}, include_bytes!({absolute:?})),\n"
        ));
    }
    generated.push_str("];\n");

    let out_dir = std::env::var("OUT_DIR").expect("set by cargo");
    let dest = Path::new(&out_dir).join("bundled_integrations.rs");
    fs::write(&dest, generated).expect("failed to write generated bundled_integrations.rs");
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, out);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("walked under root")
                .to_string_lossy()
                .replace('\\', "/");
            if relative == ".gitkeep" {
                continue;
            }
            out.push((relative, path.to_string_lossy().to_string()));
        }
    }
}
```

- [ ] **Step 5: Wire the generated registry into `mod.rs` and declare `build.rs` in `Cargo.toml`**

In `crates/infigraph-cli/Cargo.toml`, add `build = "build.rs"` under `[package]` (any line inside that table), e.g. right after the `description` line.

Replace `crates/infigraph-cli/src/artifacts/mod.rs` with:

```rust
//! Data-driven integration artifacts: bundled config fragments, hook scripts,
//! and docs for every supported agent/editor, applied via one of five
//! strategies. See docs/superpowers/specs/2026-08-09-agent-target-templates-design.md.

mod convention;
mod discovery;
mod manifest;
mod resolver;
mod step;
mod strategy;
mod template;

pub(crate) use discovery::{discover_artifacts, ResolvedArtifact};
pub(crate) use strategy::{ApplyOutcome, Strategy};

include!(concat!(env!("OUT_DIR"), "/bundled_integrations.rs"));
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts:: -- --nocapture`
Expected: all tests across `manifest`, `template`, `strategy`, `convention`, `resolver`, and all 9 `discovery` tests pass. (`BUNDLED_INTEGRATIONS` itself is an empty slice at this point, since `resources/integrations/` only has `.gitkeep` — that's expected and fine; nothing in this plan's tests reads the real constant, they all pass a synthetic `bundled` slice.)

- [ ] **Step 7: Commit**

```bash
git add crates/infigraph-cli/build.rs crates/infigraph-cli/Cargo.toml crates/infigraph-cli/src/artifacts/discovery.rs crates/infigraph-cli/src/artifacts/mod.rs crates/infigraph-cli/src/artifacts/strategy.rs
git commit -m "feat(cli): build.rs bundled registry + two-tier artifact discovery"
```

---

## Task 10: `InstallStep` classification

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/step.rs`
- Modify: `crates/infigraph-cli/src/artifacts/discovery.rs` (add `step` field to `ResolvedArtifact` and every constructor site)
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (re-export `InstallStep`)

**Interfaces:**
- Consumes: `Strategy` (Task 9), `ResolvedArtifact` (Task 9).
- Produces:
  - `pub(crate) enum InstallStep { McpRegistration, DocsAndRules, Hooks, Models }`
  - `impl InstallStep { pub const ALL: &'static [InstallStep]; pub fn classify(relative_target_path: &str, strategy: Strategy) -> InstallStep }`
  - `ResolvedArtifact` gains `pub step: InstallStep`.

- [ ] **Step 1: Write the failing tests**

Write `crates/infigraph-cli/src/artifacts/step.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::super::strategy::Strategy;
    use super::*;

    #[test]
    fn hooks_subdirectory_classifies_as_hooks() {
        assert_eq!(
            InstallStep::classify("claude-code/hooks/enforce.sh", Strategy::Overwrite),
            InstallStep::Hooks
        );
    }

    #[test]
    fn rules_subdirectory_classifies_as_docs_and_rules() {
        assert_eq!(
            InstallStep::classify("cursor/rules/infigraph.mdc", Strategy::Overwrite),
            InstallStep::DocsAndRules
        );
    }

    #[test]
    fn skills_subdirectory_classifies_as_docs_and_rules() {
        assert_eq!(
            InstallStep::classify(".claude/skills/infigraph-reindex/SKILL.md", Strategy::Overwrite),
            InstallStep::DocsAndRules
        );
    }

    #[test]
    fn marker_delimited_strategy_classifies_as_docs_and_rules_regardless_of_path() {
        assert_eq!(
            InstallStep::classify(".claude/CLAUDE.md", Strategy::MarkerDelimited),
            InstallStep::DocsAndRules
        );
    }

    #[test]
    fn plain_mcp_fragment_classifies_as_mcp_registration() {
        assert_eq!(
            InstallStep::classify(".claude.json", Strategy::JsonDeepMerge),
            InstallStep::McpRegistration
        );
        assert_eq!(
            InstallStep::classify(".codex/config.toml", Strategy::TomlSection),
            InstallStep::McpRegistration
        );
        assert_eq!(
            InstallStep::classify(".vscode/mcp.json", Strategy::JsonKeyPath),
            InstallStep::McpRegistration
        );
    }

    #[test]
    fn all_contains_every_variant_exactly_once() {
        let all = InstallStep::ALL;
        assert_eq!(all.len(), 4);
        for step in [
            InstallStep::McpRegistration,
            InstallStep::DocsAndRules,
            InstallStep::Hooks,
            InstallStep::Models,
        ] {
            assert_eq!(all.iter().filter(|s| **s == step).count(), 1);
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::step -- --nocapture`
Expected: compile error (`InstallStep` doesn't exist yet).

- [ ] **Step 3: Implement `InstallStep`**

Prepend to `crates/infigraph-cli/src/artifacts/step.rs` (above `#[cfg(test)]`):

```rust
use super::strategy::Strategy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallStep {
    /// Every mcp.json-shaped artifact across all integrations.
    McpRegistration,
    /// CLAUDE.md, editor rules, and the shared reindex skill.
    DocsAndRules,
    /// Hook scripts and their settings.json wiring.
    Hooks,
    /// Semantic search model files -- unchanged, not artifact-based.
    Models,
}

impl InstallStep {
    pub(crate) const ALL: &'static [InstallStep] = &[
        InstallStep::McpRegistration,
        InstallStep::DocsAndRules,
        InstallStep::Hooks,
        InstallStep::Models,
    ];

    /// Classifies an artifact by its destination path (for `path`/mirrored
    /// paths, not the bundled-resources source path) and strategy. A
    /// `marker_delimited` artifact is always `DocsAndRules` regardless of
    /// path (there is no other kind of marker-delimited content in this
    /// design). Otherwise: anything under a `hooks/` path segment is
    /// `Hooks`; anything under `rules/` or `skills/` is `DocsAndRules`;
    /// everything else is `McpRegistration`. This heuristic only needs to be
    /// *good enough* to group #50's future `--mode` flag -- no such flag
    /// ships yet.
    pub(crate) fn classify(relative_target_path: &str, strategy: Strategy) -> InstallStep {
        if strategy == Strategy::MarkerDelimited {
            return InstallStep::DocsAndRules;
        }
        let segments: Vec<&str> = relative_target_path.split('/').collect();
        if segments.iter().any(|s| *s == "hooks") {
            InstallStep::Hooks
        } else if segments.iter().any(|s| *s == "rules" || *s == "skills") {
            InstallStep::DocsAndRules
        } else {
            InstallStep::McpRegistration
        }
    }
}
```

- [ ] **Step 4: Run the `step` tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::step -- --nocapture`
Expected: all 6 tests pass.

- [ ] **Step 5: Add the `step` field to `ResolvedArtifact` and every construction site**

In `crates/infigraph-cli/src/artifacts/discovery.rs`:

1. Add the import: change `use super::strategy::Strategy;` to also bring in nothing extra (already imports `Strategy`); add `use super::step::InstallStep;` alongside the other `use super::...` lines.
2. Add the field to the struct definition, just above the closing `}` of `ResolvedArtifact`:
   ```rust
       pub step: InstallStep,
   ```
3. In the Pass 1 (`artifacts.push(ResolvedArtifact { ... })`) constructor inside the manifest loop, add:
   ```rust
       step: InstallStep::classify(&entry.path, strategy),
   ```
   as the last field before the closing `}`.
4. In the Pass 2 (`artifacts.push(ResolvedArtifact { ... })`) constructor inside the convention-based loop, add:
   ```rust
       step: InstallStep::classify(relative_path, strategy),
   ```
   as the last field before the closing `}`.

- [ ] **Step 6: Update `discovery.rs`'s tests to assert on `step` where it matters**

Add one new test inside `crates/infigraph-cli/src/artifacts/discovery.rs`'s existing `#[cfg(test)] mod tests { ... }` block:

```rust
    #[test]
    fn discovered_artifacts_get_correct_install_step() {
        let bundled: &[(&str, &[u8])] = &[
            ("claude-code/hooks/enforce.sh", b"#!/bin/bash\n"),
            ("claude-code/.claude.json", br#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}"}}}"#),
        ];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        let hook = artifacts
            .iter()
            .find(|a| a.target_relative_path == "hooks/enforce.sh")
            .unwrap();
        assert_eq!(hook.step, super::super::step::InstallStep::Hooks);

        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path == ".claude.json")
            .unwrap();
        assert_eq!(mcp.step, super::super::step::InstallStep::McpRegistration);
    }
```

- [ ] **Step 7: Expose `InstallStep` from the module root**

In `crates/infigraph-cli/src/artifacts/mod.rs`, change:

```rust
pub(crate) use discovery::{discover_artifacts, ResolvedArtifact};
pub(crate) use strategy::{ApplyOutcome, Strategy};
```

to:

```rust
pub(crate) use discovery::{discover_artifacts, ResolvedArtifact};
pub(crate) use step::InstallStep;
pub(crate) use strategy::{ApplyOutcome, Strategy};
```

- [ ] **Step 8: Run every `artifacts` test to verify everything still passes**

Run: `cargo test -p infigraph-cli artifacts:: -- --nocapture`
Expected: all tests pass, including the new `discovered_artifacts_get_correct_install_step` test and the 6 `step` tests.

- [ ] **Step 9: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/step.rs crates/infigraph-cli/src/artifacts/discovery.rs crates/infigraph-cli/src/artifacts/mod.rs
git commit -m "feat(cli): InstallStep classification, groundwork for issue #50"
```

---

## Task 11: Uninstall dispatch + end-to-end integration test

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs`

**Interfaces:**
- Consumes: every `apply_*`/`remove_*` function from Tasks 4-8, `ResolvedArtifact`/`Strategy`/`InstallStep` from Tasks 9-10.
- Produces:
  - `pub(crate) fn apply_resolved_artifact(artifact: &ResolvedArtifact, home: &std::path::Path) -> anyhow::Result<ApplyOutcome>`
  - `pub(crate) fn remove_resolved_artifact(artifact: &ResolvedArtifact, home: &std::path::Path) -> anyhow::Result<bool>`

These are the two functions Plan 2's `cmd_install`/`cmd_uninstall` will call per discovered artifact — this task is where discovery, strategy dispatch, and the five apply/remove implementations are proven to work together end to end, still without touching any existing code.

- [ ] **Step 1: Write the failing end-to-end test**

Append a new `#[cfg(test)] mod integration_tests { ... }` block at the end of `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn full_discover_apply_verify_uninstall_verify_removed_cycle() {
        let bundled: &[(&str, &[u8])] = &[
            (
                "claude-code/.claude.json",
                br#"{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}"#,
            ),
            (
                "claude-code/.claude/hooks/infigraph-enforce.sh",
                b"#!/usr/bin/env bash\necho enforce\n",
            ),
            (
                "claude-code/config.toml",
                br#"label = "Claude Code"

[[artifact]]
path = ".claude/CLAUDE.md"
strategy = "marker_delimited"
start = "<!-- infigraph-primary-search -->"
end = "<!-- /infigraph-primary-search -->"
content_file = "../shared/agents.md"
"#,
            ),
            ("shared/agents.md", b"## Infigraph instructions\nUse infigraph tools first."),
            (
                "codex/config.toml",
                br#"label = "Codex"

[[artifact]]
path = ".codex/config.toml"
strategy = "toml_section"
key_path = ["mcp_servers", "infigraph"]
content_file = "mcp-section.toml"
"#,
            ),
            ("codex/mcp-section.toml", b"command = \"{{mcp_path}}\"\nargs = [\"--mcp\"]"),
        ];

        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(bundled, user_dir.path(), mcp_path).unwrap();
        assert_eq!(artifacts.len(), 4);

        for artifact in &artifacts {
            let outcome = apply_resolved_artifact(artifact, home).unwrap();
            assert!(matches!(outcome, ApplyOutcome::Written), "{:?} failed to apply", artifact.target_relative_path);
        }

        // Verify each landed at the expected real path with expected content --
        // .claude.json at $HOME directly (no .claude/ nesting, matching the
        // CLAUDE_CODE_SPECIAL destination it replaces), hooks nested under
        // .claude/hooks/ (stripped from the bundled claude-code/.claude/hooks/ tree).
        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(claude_json["mcpServers"]["infigraph"]["command"], mcp_path);

        let hook =
            std::fs::read_to_string(home.join(".claude/hooks/infigraph-enforce.sh")).unwrap();
        assert!(hook.contains("echo enforce"));

        let claude_md = std::fs::read_to_string(home.join(".claude/CLAUDE.md")).unwrap();
        assert!(claude_md.contains("Use infigraph tools first."));

        let codex_toml = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
        assert!(codex_toml.contains(mcp_path));
        assert!(codex_toml.contains("[mcp_servers.infigraph]"));

        // Reapply is idempotent (no duplication) -- exercises the whole
        // pipeline's self-healing property, not just one strategy in isolation.
        for artifact in &artifacts {
            apply_resolved_artifact(artifact, home).unwrap();
        }
        let claude_json_again: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            claude_json_again["mcpServers"]["infigraph"]["args"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // Uninstall every artifact, verify each is actually gone.
        for artifact in &artifacts {
            let removed = remove_resolved_artifact(artifact, home).unwrap();
            assert!(removed, "{:?} was not removed", artifact.target_relative_path);
        }

        let claude_json_after: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert!(claude_json_after["mcpServers"]["infigraph"].is_null());
        assert!(!home.join(".claude/hooks/infigraph-enforce.sh").exists());
        assert!(!std::fs::read_to_string(home.join(".claude/CLAUDE.md"))
            .unwrap()
            .contains("Use infigraph tools first."));
        assert!(!std::fs::read_to_string(home.join(".codex/config.toml"))
            .unwrap()
            .contains("[mcp_servers.infigraph]"));
    }

    #[test]
    fn user_override_end_to_end_replaces_bundled_content() {
        let bundled: &[(&str, &[u8])] = &[(
            "claude-code/.claude/hooks/infigraph-enforce.sh",
            b"#!/usr/bin/env bash\necho bundled\n",
        )];
        let user_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(user_dir.path().join("claude-code/.claude/hooks")).unwrap();
        std::fs::write(
            user_dir.path().join("claude-code/.claude/hooks/infigraph-enforce.sh"),
            "#!/usr/bin/env bash\necho overridden\n",
        )
        .unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/bin/infigraph-mcp";

        let artifacts = discover_artifacts(bundled, user_dir.path(), mcp_path).unwrap();
        for artifact in &artifacts {
            apply_resolved_artifact(artifact, home_dir.path()).unwrap();
        }

        let content = std::fs::read_to_string(
            home_dir.path().join(".claude/hooks/infigraph-enforce.sh"),
        )
        .unwrap();
        assert!(content.contains("echo overridden"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::integration_tests -- --nocapture`
Expected: compile error (`apply_resolved_artifact`, `remove_resolved_artifact` don't exist yet).

- [ ] **Step 3: Implement the dispatch functions**

Add to `crates/infigraph-cli/src/artifacts/mod.rs`, after the `include!(...)` line and before the `#[cfg(test)]` block added in Step 1:

```rust
pub(crate) fn apply_resolved_artifact(
    artifact: &ResolvedArtifact,
    home: &std::path::Path,
) -> anyhow::Result<ApplyOutcome> {
    let target_path = home.join(&artifact.target_relative_path);
    match artifact.strategy {
        Strategy::JsonDeepMerge => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("json_deep_merge artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            strategy::apply_json_deep_merge(&target_path, text)
        }
        Strategy::Overwrite => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("overwrite artifact has no content"))?;
            strategy::apply_overwrite(&target_path, content)
        }
        Strategy::MarkerDelimited => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            let start = artifact
                .start
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing start marker"))?;
            let end = artifact
                .end
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing end marker"))?;
            strategy::apply_marker_delimited(&target_path, start, end, text)
        }
        Strategy::TomlSection => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact missing key_path"))?;
            strategy::apply_toml_section(&target_path, key_path, text)
        }
        Strategy::JsonKeyPath => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact missing key_path"))?;
            strategy::apply_json_key_path(&target_path, key_path, text)
        }
    }
}

pub(crate) fn remove_resolved_artifact(
    artifact: &ResolvedArtifact,
    home: &std::path::Path,
) -> anyhow::Result<bool> {
    let target_path = home.join(&artifact.target_relative_path);
    match artifact.strategy {
        Strategy::JsonDeepMerge => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("json_deep_merge artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            strategy::remove_json_deep_merge(&target_path, text)
        }
        Strategy::Overwrite => strategy::remove_overwrite(&target_path),
        Strategy::MarkerDelimited => {
            let start = artifact
                .start
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing start marker"))?;
            let end = artifact
                .end
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing end marker"))?;
            strategy::remove_marker_delimited(&target_path, start, end)
        }
        Strategy::TomlSection => {
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact missing key_path"))?;
            strategy::remove_toml_section(&target_path, key_path)
        }
        Strategy::JsonKeyPath => {
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact missing key_path"))?;
            strategy::remove_json_key_path(&target_path, key_path)
        }
    }
}
```

Also change the `mod`/`use` block at the top of `crates/infigraph-cli/src/artifacts/mod.rs` so `strategy`'s free functions (not just its re-exported types) are reachable as `strategy::apply_json_deep_merge` etc. — this already works as written, since `mod strategy;` (not `mod strategy { ... }` inline) makes `strategy::` a valid path from within the same file; no further change needed there.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts:: -- --nocapture`
Expected: every test in the `artifacts` module tree passes, including both `integration_tests`.

- [ ] **Step 5: Run the full crate test suite and lints**

Run: `cargo test -p infigraph-cli`
Expected: all tests pass (existing `config_targets`/`hooks` tests are untouched by this plan and still pass).

Run: `cargo fmt --all -- --check`
Expected: no diff. If there is one, run `cargo fmt --all` and re-check.

Run: `cargo clippy -p infigraph-cli --all-targets -- -D warnings`
Expected: no warnings. Fix any that appear before continuing.

- [ ] **Step 6: Commit**

```bash
git add crates/infigraph-cli/src/artifacts/mod.rs
git commit -m "feat(cli): artifact apply/remove dispatch, end-to-end engine test"
```

---

## Task 12: Make resolver-driven artifacts (VS Code, Zed) actually work

Tasks 2, 9, and 11 built the engine around every artifact having a static `path` known at discovery time. That's wrong for VS Code and Zed: the whole reason they need a `resolver` is that **no fixed path exists to put in the manifest** (see the design spec's "Path resolution and the resolver escape hatch" — Zed's own example manifest has no `path` field at all). As written, `ArtifactEntry.path` is a required `String`, so a resolver-only manifest entry would fail to parse; and even if it parsed, `apply_resolved_artifact` never calls the resolver at all. This task fixes both, plus the fact that a bundled resolver script is embedded bytes in the binary, not a real file — it must be materialized to disk before it can be spawned as a subprocess.

**Files:**
- Modify: `crates/infigraph-cli/src/artifacts/manifest.rs` (from Task 2)
- Modify: `crates/infigraph-cli/src/artifacts/discovery.rs` (from Task 9)
- Modify: `crates/infigraph-cli/src/artifacts/resolver.rs` (from Task 8)
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (from Task 11)

**Interfaces:**
- Consumes: `run_resolver`/`ResolverOutput`/`ResolverData` (Task 8), `Strategy`/`ApplyOutcome` (Task 4-9), `InstallStep` (Task 10).
- Produces:
  - `ArtifactEntry.path` becomes `Option<String>` (was `String`).
  - `ResolvedArtifact.target_relative_path` becomes `Option<String>` (was `String`); `ResolvedArtifact.resolver` becomes `Option<ResolverSpec>` (was `Option<(Vec<String>, PathBuf)>`).
  - `pub(crate) struct ResolverSpec { pub script_relative_path: String, pub script_bytes: Vec<u8>, pub script_filename: String, pub extra_args: Vec<String> }`
  - `pub(crate) fn run_resolver_from_script(script_bytes: &[u8], script_filename: &str, extra_args: &[String], mcp_path: &str, home: &std::path::Path) -> anyhow::Result<ResolverOutput>`
  - `apply_resolved_artifact`/`remove_resolved_artifact` gain a third parameter: `mcp_path: &str`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests { ... }` block in `crates/infigraph-cli/src/artifacts/manifest.rs`, replacing the existing `rejects_artifact_missing_required_path` test (which asserted the opposite of the now-correct behavior) with:

```rust
    #[test]
    fn resolver_only_entry_with_no_path_parses_successfully() {
        let toml = r#"
[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]
"#;
        let manifest = parse_manifest(toml).expect("resolver-only entries have no static path");
        assert!(manifest.artifacts[0].path.is_none());
        assert_eq!(
            manifest.artifacts[0].resolver,
            Some(vec!["./resolve-zed-path.sh".to_string()])
        );
    }
```

Also update `parses_minimal_manifest_with_one_artifact`'s assertion `assert_eq!(a.path, ".claude/CLAUDE.md");` to `assert_eq!(a.path.as_deref(), Some(".claude/CLAUDE.md"));`, and `parses_resolver_field`'s fixture TOML: remove the line `path = "unused-for-resolver-based-artifacts"` entirely, and add `assert!(manifest.artifacts[0].path.is_none());` after its existing assertions.

Add to the `#[cfg(test)] mod tests { ... }` block in `crates/infigraph-cli/src/artifacts/discovery.rs`:

```rust
    #[test]
    fn resolver_only_manifest_entry_has_no_static_target_path() {
        let bundled: &[(&str, &[u8])] = &[
            (
                "zed/config.toml",
                br#"label = "Zed"

[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]
"#,
            ),
            ("zed/resolve-zed-path.sh", b"#!/usr/bin/env bash\necho resolver\n"),
        ];
        let user_dir = tempfile::tempdir().unwrap();

        let artifacts = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp").unwrap();

        assert_eq!(artifacts.len(), 1);
        let a = &artifacts[0];
        assert!(a.target_relative_path.is_none());
        assert!(a.content.is_none());
        let resolver = a.resolver.as_ref().expect("should have a resolver spec");
        assert_eq!(resolver.script_filename, "resolve-zed-path.sh");
        assert_eq!(resolver.script_bytes, b"#!/usr/bin/env bash\necho resolver\n");
        assert!(resolver.extra_args.is_empty());
        assert_eq!(a.step, super::super::step::InstallStep::McpRegistration);
    }

    #[test]
    fn manifest_entry_missing_both_path_and_resolver_is_an_error() {
        let bundled: &[(&str, &[u8])] = &[(
            "broken/config.toml",
            br#"[[artifact]]
strategy = "overwrite"
"#,
        )];
        let user_dir = tempfile::tempdir().unwrap();
        let result = discover_artifacts(bundled, user_dir.path(), "/bin/infigraph-mcp");
        assert!(result.is_err(), "an artifact with neither path nor resolver is a manifest bug");
    }
```

Also fix every existing assertion in `discovery.rs` that compares `target_relative_path` to a bare `&str` (a compile error once the field becomes `Option<String>`) — these now use the corrected, prefix-stripped values already fixed in Task 9:
- `discovers_convention_based_json_artifact_from_bundled`: `assert_eq!(a.target_relative_path, ".claude.json", ...);` → `assert_eq!(a.target_relative_path.as_deref(), Some(".claude.json"), ...);`
- `manifest_entry_produces_explicit_artifact_with_all_fields`: `assert_eq!(a.target_relative_path, ".codex/config.toml");` → `assert_eq!(a.target_relative_path.as_deref(), Some(".codex/config.toml"));`
- `user_override_adds_a_new_convention_based_file_with_zero_manifest_changes`: `.any(|a| a.target_relative_path == "hooks/new-hook.sh")` → `.any(|a| a.target_relative_path.as_deref() == Some("hooks/new-hook.sh"))`
- `user_override_adds_a_wholly_new_integration_directory`: `assert_eq!(artifacts[0].target_relative_path, ".custom/mcp.json");` → `assert_eq!(artifacts[0].target_relative_path.as_deref(), Some(".custom/mcp.json"));`
- `discovered_artifacts_get_correct_install_step` (added in Task 10): both `.find(|a| a.target_relative_path == "...")` closures (now `"hooks/enforce.sh"` and `".claude.json"`) → `.find(|a| a.target_relative_path.as_deref() == Some("..."))`

Add to the `#[cfg(test)] mod tests { ... }` block in `crates/infigraph-cli/src/artifacts/resolver.rs`:

```rust
    #[test]
    fn run_resolver_from_script_materializes_bundled_bytes_and_executes() {
        let script = b"#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"ok\",\"data\":{\"path\":\"/resolved/settings.json\"}}\nEOF\n";

        let output = run_resolver_from_script(
            script,
            "resolve-zed-path.sh",
            &[],
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => assert_eq!(data.path, "/resolved/settings.json"),
            other => panic!("expected Ok, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts:: -- --nocapture`
Expected: compile errors throughout `manifest.rs`/`discovery.rs` (type mismatches between `String` and the not-yet-`Option` fields) and a missing-function error for `run_resolver_from_script`.

- [ ] **Step 3: Make `ArtifactEntry.path` optional**

In `crates/infigraph-cli/src/artifacts/manifest.rs`, change:

```rust
    pub path: String,
```

to:

```rust
    #[serde(default)]
    pub path: Option<String>,
```

- [ ] **Step 4: Add `ResolverSpec` and rework discovery's resolver/path resolution**

In `crates/infigraph-cli/src/artifacts/discovery.rs`:

1. Change the `ResolvedArtifact` struct's two affected fields:

```rust
    pub target_relative_path: Option<String>,
```

(replacing the old `pub target_relative_path: String,`), and:

```rust
    pub resolver: Option<ResolverSpec>,
```

(replacing the old `pub resolver: Option<(Vec<String>, PathBuf)>,` and its doc comment).

2. Add the new struct, just above `ResolvedArtifact`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolverSpec {
    /// The script's normalized path within the bundled/user-override
    /// registry, e.g. "zed/resolve-zed-path.sh" -- kept for error messages.
    pub script_relative_path: String,
    pub script_bytes: Vec<u8>,
    /// Just the filename (e.g. "resolve-zed-path.sh"), used when writing the
    /// script to a temp directory before spawning it.
    pub script_filename: String,
    pub extra_args: Vec<String>,
}
```

3. Add a strategy-based template-format helper, just above `resolve_content_file`:

```rust
fn template_format_for_strategy(strategy: Strategy) -> Option<TemplateFormat> {
    match strategy {
        Strategy::JsonDeepMerge | Strategy::JsonKeyPath => Some(TemplateFormat::Json),
        Strategy::TomlSection => Some(TemplateFormat::Toml),
        Strategy::Overwrite | Strategy::MarkerDelimited => None,
    }
}
```

(The existing `template_format_for(relative_path)`, extension-based, stays as-is and keeps being used by Pass 2's convention-based files below, which always have a real path.)

4. Replace the Pass 1 manifest loop's artifact-construction body (from `for entry in &manifest.artifacts {` through the closing `artifacts.push(ResolvedArtifact { ... });` for that loop, as written in Tasks 9-10) with:

```rust
        for entry in &manifest.artifacts {
            let strategy = Strategy::parse(&entry.strategy)
                .with_context(|| format!("in manifest {relative_path}, artifact declaration"))?;

            anyhow::ensure!(
                entry.path.is_some() || entry.resolver.is_some(),
                "in manifest {relative_path}: artifact must declare either \"path\" or \"resolver\""
            );

            let resolver_spec = match &entry.resolver {
                Some(resolver_cmd) => {
                    anyhow::ensure!(
                        !resolver_cmd.is_empty(),
                        "in manifest {relative_path}: resolver command must not be empty"
                    );
                    let script_arg = &resolver_cmd[0];
                    let script_relative = script_arg.strip_prefix("./").unwrap_or(script_arg);
                    let combined = format!("{integration_dir}/{script_relative}");
                    let normalized = normalize_relative_path(&combined);
                    let script_bytes = files.get(&normalized).cloned().with_context(|| {
                        format!("resolver script \"{script_arg}\" (resolved to \"{normalized}\") not found")
                    })?;
                    manifest_claimed.insert(normalized.clone());
                    let script_filename = Path::new(&normalized)
                        .file_name()
                        .expect("resolver script path must have a filename")
                        .to_string_lossy()
                        .to_string();
                    Some(ResolverSpec {
                        script_relative_path: normalized,
                        script_bytes,
                        script_filename,
                        extra_args: resolver_cmd[1..].to_vec(),
                    })
                }
                None => None,
            };

            let content = match &entry.content_file {
                Some(content_file) => {
                    let raw = resolve_content_file(&files, integration_dir, content_file)?;
                    let normalized_content_file =
                        normalize_relative_path(&format!("{integration_dir}/{content_file}"));
                    manifest_claimed.insert(normalized_content_file);
                    let text = String::from_utf8(raw)
                        .with_context(|| format!("content_file \"{content_file}\" is not valid UTF-8"))?;
                    // Format is derived from the artifact's *strategy*, not its
                    // (possibly absent) path -- a resolver-only artifact like
                    // VS Code's has no static path but still needs a JSON
                    // template substitution for its local content_file.
                    let substituted = match template_format_for_strategy(strategy) {
                        Some(format) => substitute_mcp_path(&text, mcp_path, format),
                        None => text,
                    };
                    Some(substituted.into_bytes())
                }
                None => None,
            };

            let step = match &entry.path {
                Some(path) => InstallStep::classify(path, strategy),
                // Both current resolver-driven artifacts (VS Code, Zed) are
                // MCP registrations; revisit if a future resolver-driven
                // artifact is ever a hook or doc/rules entry instead.
                None => InstallStep::McpRegistration,
            };

            artifacts.push(ResolvedArtifact {
                integration_label: label.clone(),
                target_relative_path: entry.path.clone(),
                strategy,
                content,
                start: entry.start.clone(),
                end: entry.end.clone(),
                key_path: entry.key_path.clone(),
                resolver: resolver_spec,
                step,
            });
        }
```

5. Add `use super::step::InstallStep;` to the top of `discovery.rs` if not already present from Task 10.

- [ ] **Step 5: Implement `run_resolver_from_script`**

Add to `crates/infigraph-cli/src/artifacts/resolver.rs`, just above `#[cfg(test)]`:

```rust
/// Writes a bundled/user-override resolver script's bytes to a temp file,
/// makes it executable, spawns it via `run_resolver`, and cleans up
/// afterward. Needed because bundled content is embedded bytes in the
/// binary, not a real file on disk -- `run_resolver` alone can only spawn a
/// command that already exists at some real `cwd`.
pub(crate) fn run_resolver_from_script(
    script_bytes: &[u8],
    script_filename: &str,
    extra_args: &[String],
    mcp_path: &str,
    home: &Path,
) -> Result<ResolverOutput> {
    let tmp = tempfile::tempdir().context("failed to create temp directory for resolver script")?;
    let script_path = tmp.path().join(script_filename);
    std::fs::write(&script_path, script_bytes)
        .with_context(|| format!("failed to write resolver script to {}", script_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to make {} executable", script_path.display()))?;
    }

    let mut resolver_cmd = vec![format!("./{script_filename}")];
    resolver_cmd.extend(extra_args.iter().cloned());

    run_resolver(&resolver_cmd, tmp.path(), mcp_path, home)
}
```

- [ ] **Step 6: Rework `apply_resolved_artifact`/`remove_resolved_artifact` to resolve via the resolver when present**

In `crates/infigraph-cli/src/artifacts/mod.rs`, replace the entire `apply_resolved_artifact` function (written in Task 11) with:

```rust
pub(crate) fn apply_resolved_artifact(
    artifact: &ResolvedArtifact,
    home: &std::path::Path,
    mcp_path: &str,
) -> anyhow::Result<ApplyOutcome> {
    let (target_path, resolved_content) = match &artifact.resolver {
        Some(spec) => {
            let output = resolver::run_resolver_from_script(
                &spec.script_bytes,
                &spec.script_filename,
                &spec.extra_args,
                mcp_path,
                home,
            )
            .with_context(|| format!("running resolver {}", spec.script_relative_path))?;
            match output {
                resolver::ResolverOutput::Ok { data } => {
                    let content = match data.content {
                        Some(v) => Some(serde_json::to_vec(&v)?),
                        None => artifact.content.clone(),
                    };
                    (std::path::PathBuf::from(data.path), content)
                }
                resolver::ResolverOutput::Skip { message } => {
                    return Ok(ApplyOutcome::Skipped {
                        reason: format!("resolver reported skip: {message}"),
                        manual_snippet: String::new(),
                    });
                }
                resolver::ResolverOutput::Error { message } => {
                    anyhow::bail!(
                        "resolver {} failed: {message}",
                        spec.script_relative_path
                    );
                }
            }
        }
        None => {
            let relative = artifact.target_relative_path.as_ref().ok_or_else(|| {
                anyhow::anyhow!("artifact has neither a static path nor a resolver")
            })?;
            (home.join(relative), artifact.content.clone())
        }
    };

    match artifact.strategy {
        Strategy::JsonDeepMerge => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("json_deep_merge artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            strategy::apply_json_deep_merge(&target_path, text)
        }
        Strategy::Overwrite => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("overwrite artifact has no content"))?;
            strategy::apply_overwrite(&target_path, &content)
        }
        Strategy::MarkerDelimited => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            let start = artifact
                .start
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing start marker"))?;
            let end = artifact
                .end
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing end marker"))?;
            strategy::apply_marker_delimited(&target_path, start, end, text)
        }
        Strategy::TomlSection => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact missing key_path"))?;
            strategy::apply_toml_section(&target_path, key_path, text)
        }
        Strategy::JsonKeyPath => {
            let content = resolved_content
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact has no content"))?;
            let text = std::str::from_utf8(&content)?;
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact missing key_path"))?;
            strategy::apply_json_key_path(&target_path, key_path, text)
        }
    }
}
```

Add `use anyhow::Context;` to the top of `mod.rs` if not already present (needed for `.with_context` above).

Replace the entire `remove_resolved_artifact` function (written in Task 11) with:

```rust
pub(crate) fn remove_resolved_artifact(
    artifact: &ResolvedArtifact,
    home: &std::path::Path,
    mcp_path: &str,
) -> anyhow::Result<bool> {
    let target_path = match &artifact.resolver {
        Some(spec) => {
            let output = resolver::run_resolver_from_script(
                &spec.script_bytes,
                &spec.script_filename,
                &spec.extra_args,
                mcp_path,
                home,
            )
            .with_context(|| format!("running resolver {}", spec.script_relative_path))?;
            match output {
                resolver::ResolverOutput::Ok { data } => std::path::PathBuf::from(data.path),
                resolver::ResolverOutput::Skip { .. } => return Ok(false),
                resolver::ResolverOutput::Error { message } => {
                    anyhow::bail!("resolver {} failed: {message}", spec.script_relative_path);
                }
            }
        }
        None => {
            let relative = artifact.target_relative_path.as_ref().ok_or_else(|| {
                anyhow::anyhow!("artifact has neither a static path nor a resolver")
            })?;
            home.join(relative)
        }
    };

    match artifact.strategy {
        Strategy::JsonDeepMerge => {
            let content = artifact
                .content
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("json_deep_merge artifact has no content"))?;
            let text = std::str::from_utf8(content)?;
            strategy::remove_json_deep_merge(&target_path, text)
        }
        Strategy::Overwrite => strategy::remove_overwrite(&target_path),
        Strategy::MarkerDelimited => {
            let start = artifact
                .start
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing start marker"))?;
            let end = artifact
                .end
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("marker_delimited artifact missing end marker"))?;
            strategy::remove_marker_delimited(&target_path, start, end)
        }
        Strategy::TomlSection => {
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("toml_section artifact missing key_path"))?;
            strategy::remove_toml_section(&target_path, key_path)
        }
        Strategy::JsonKeyPath => {
            let key_path = artifact
                .key_path
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("json_key_path artifact missing key_path"))?;
            strategy::remove_json_key_path(&target_path, key_path)
        }
    }
}
```

- [ ] **Step 7: Fix the Task 11 integration tests' call sites for the new `mcp_path` parameter**

In `crates/infigraph-cli/src/artifacts/mod.rs`'s `integration_tests` module (from Task 11), every call to `apply_resolved_artifact(artifact, home)` becomes `apply_resolved_artifact(artifact, home, mcp_path)`, and every `remove_resolved_artifact(artifact, home)` becomes `remove_resolved_artifact(artifact, home, mcp_path)` (both tests already have an `mcp_path` variable in scope from discovery — reuse it, don't hardcode a new literal).

- [ ] **Step 8: Add an end-to-end resolver test**

Append to the `#[cfg(test)] mod integration_tests { ... }` block in `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
    #[test]
    fn resolver_driven_artifact_applies_to_resolver_computed_path() {
        let bundled: &[(&str, &[u8])] = &[
            (
                "zed/config.toml",
                br#"label = "Zed"

[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.sh"]
"#,
            ),
            (
                "zed/resolve-zed-path.sh",
                b"#!/usr/bin/env bash\ninput=$(cat)\nmcp=$(echo \"$input\" | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"mcp_path\"])')\necho \"{\\\"status\\\":\\\"ok\\\",\\\"data\\\":{\\\"path\\\":\\\"zed-settings.json\\\",\\\"content\\\":{\\\"command\\\":\\\"$mcp\\\",\\\"args\\\":[\\\"--mcp\\\"],\\\"env\\\":{}}}}\"\n",
            ),
        ];
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(bundled, user_dir.path(), mcp_path).unwrap();
        assert_eq!(artifacts.len(), 1);

        let outcome = apply_resolved_artifact(&artifacts[0], home_dir.path(), mcp_path).unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join("zed-settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["context_servers"]["infigraph"]["command"], mcp_path);

        let removed = remove_resolved_artifact(&artifacts[0], home_dir.path(), mcp_path).unwrap();
        assert!(removed);
        let after: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join("zed-settings.json")).unwrap(),
        )
        .unwrap();
        assert!(after["context_servers"]["infigraph"].is_null());
    }
```

This test requires `python3` on the test runner's `PATH` (consistent with this repo's existing hook scripts, which already assume `python3` is available — see `hooks.rs`'s `SESSION_START_HOOK_SCRIPT`). If `python3` isn't available in CI, simplify the fake resolver script to `echo` a fixed JSON literal with the `mcp_path` value hardcoded to match the test's `mcp_path` constant instead of reading it from stdin.

- [ ] **Step 9: Run every `artifacts` test to verify everything passes**

Run: `cargo test -p infigraph-cli artifacts:: -- --nocapture`
Expected: every test across all seven submodules passes, including the 2 new `manifest` tests, 2 new `discovery` tests (plus the 5 fixed pre-existing ones), 1 new `resolver` test, and 1 new `integration_tests` test.

- [ ] **Step 10: Run the full crate suite and lints**

Run: `cargo test -p infigraph-cli`
Expected: all tests pass.

Run: `cargo fmt --all -- --check`
Expected: no diff.

Run: `cargo clippy -p infigraph-cli --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 11: Commit**

```bash
git add crates/infigraph-cli/src/artifacts
git commit -m "fix(cli): make resolver-driven artifacts (VS Code, Zed) actually resolvable"
```

---

## Task 13: Five simple convention-based integrations (Gemini CLI, OpenCode, Aider, Kiro, GitHub Copilot CLI)

These five integrations need no `config.toml` manifest at all — each is a single bundled `.json` fragment at its real, mirrored destination path (the "core idea" convention: a `.json` file's mere presence at a mirrored path is its own registration). Paths and shapes are the ones independently re-verified in the design spec's "Research: verified per-agent MCP schemas" table, and (per this session's live research) VS Code and GitHub Copilot CLI's configs remain genuinely separate, so Copilot CLI's own dedicated path/shape stands as designed.

**Files:**
- Create: `crates/infigraph-cli/resources/integrations/gemini-cli/.gemini/settings.json`
- Create: `crates/infigraph-cli/resources/integrations/opencode/.config/opencode/opencode.json`
- Create: `crates/infigraph-cli/resources/integrations/aider/.aider/mcp.json`
- Create: `crates/infigraph-cli/resources/integrations/kiro/.kiro/settings/mcp.json`
- Create: `crates/infigraph-cli/resources/integrations/github-copilot-cli/.copilot/mcp-config.json`
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (fixture tests appended to `integration_tests`)

**Interfaces:**
- Consumes: `discover_artifacts`, `apply_resolved_artifact`, `Strategy` (Tasks 9-12), and — for the first time — the real compiled `BUNDLED_INTEGRATIONS` registry from `build.rs` (Task 9), instead of a synthetic fixture.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod integration_tests { ... }` block in `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
    #[test]
    fn bundled_gemini_cli_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let gemini = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".gemini/settings.json"))
            .expect("gemini-cli fragment should be discovered from the bundled registry");
        assert_eq!(gemini.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(gemini, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }

    #[test]
    fn bundled_opencode_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let opencode = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".config/opencode/opencode.json"))
            .expect("opencode fragment should be discovered from the bundled registry");
        assert_eq!(opencode.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(opencode, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".config/opencode/opencode.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcp"]["infigraph"]["type"], "local");
        assert_eq!(written["mcp"]["infigraph"]["command"][0], mcp_path);
        assert_eq!(written["mcp"]["infigraph"]["command"][1], "--mcp");
    }

    #[test]
    fn bundled_aider_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let aider = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".aider/mcp.json"))
            .expect("aider fragment should be discovered from the bundled registry");
        assert_eq!(aider.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(aider, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".aider/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }

    #[test]
    fn bundled_kiro_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let kiro = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".kiro/settings/mcp.json"))
            .expect("kiro fragment should be discovered from the bundled registry");
        assert_eq!(kiro.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(kiro, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".kiro/settings/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }

    #[test]
    fn bundled_github_copilot_cli_mcp_fragment_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let copilot = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".copilot/mcp-config.json"))
            .expect("github-copilot-cli fragment should be discovered from the bundled registry");
        assert_eq!(copilot.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(copilot, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".copilot/mcp-config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["mcpServers"]["infigraph"]["args"][0], "--mcp");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::integration_tests::bundled -- --nocapture`
Expected: all 5 tests fail on the `.expect(...)` (`.find(...)` returns `None`) — the bundled files don't exist yet.

- [ ] **Step 3: Create the five bundled fragment files**

Write `crates/infigraph-cli/resources/integrations/gemini-cli/.gemini/settings.json`:

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

Write `crates/infigraph-cli/resources/integrations/opencode/.config/opencode/opencode.json`:

```json
{"mcp":{"infigraph":{"type":"local","command":["{{mcp_path}}","--mcp"]}}}
```

Write `crates/infigraph-cli/resources/integrations/aider/.aider/mcp.json`:

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

Write `crates/infigraph-cli/resources/integrations/kiro/.kiro/settings/mcp.json`:

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

Write `crates/infigraph-cli/resources/integrations/github-copilot-cli/.copilot/mcp-config.json`:

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::integration_tests::bundled -- --nocapture`
Expected: all 5 tests pass. (`cargo test` triggers `build.rs` to re-walk `resources/integrations/` automatically, per its `cargo:rerun-if-changed` directive from Task 9 — no separate build step needed.)

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/resources/integrations/gemini-cli crates/infigraph-cli/resources/integrations/opencode crates/infigraph-cli/resources/integrations/aider crates/infigraph-cli/resources/integrations/kiro crates/infigraph-cli/resources/integrations/github-copilot-cli crates/infigraph-cli/src/artifacts/mod.rs
git commit -m "feat(cli): bundle MCP config for Gemini CLI, OpenCode, Aider, Kiro, GitHub Copilot CLI"
```

---

## Task 14: Claude Code (hooks, settings.json, .claude.json, CLAUDE.md, and the shared reindex skill)

The most complex integration: 10 hook scripts, a `settings.json` fragment wiring all of them into their exact current event/matcher/timeout/async shape, the `.claude.json` MCP registration, and — via `config.toml` — CLAUDE.md's marker-delimited instructional block and the shared reindex skill, both pulled from `shared/`.

**Files:**
- Create: `crates/infigraph-cli/resources/integrations/claude-code/.claude.json`
- Create: `crates/infigraph-cli/resources/integrations/claude-code/.claude/settings.json`
- Create: `crates/infigraph-cli/resources/integrations/claude-code/.claude/hooks/infigraph-enforce.sh` (and 9 more — see Step 3)
- Create: `crates/infigraph-cli/resources/integrations/shared/agents.md`
- Create: `crates/infigraph-cli/resources/integrations/shared/skills/infigraph-reindex/SKILL.md`
- Create: `crates/infigraph-cli/resources/integrations/claude-code/config.toml`
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (fixture tests)

**Interfaces:**
- Consumes: everything from Tasks 1-13.

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod integration_tests { ... }` block in `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
    #[test]
    fn bundled_claude_json_applies_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let claude_json = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude.json"))
            .expect("claude-code's .claude.json fragment should be discovered");
        assert_eq!(claude_json.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(claude_json, home_dir.path(), mcp_path).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn bundled_claude_md_and_reindex_skill_apply_via_manifest() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let claude_md = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude/CLAUDE.md"))
            .expect("CLAUDE.md marker_delimited artifact should be discovered from claude-code/config.toml");
        assert_eq!(claude_md.strategy, Strategy::MarkerDelimited);
        apply_resolved_artifact(claude_md, home_dir.path(), mcp_path).unwrap();
        let claude_md_content =
            std::fs::read_to_string(home_dir.path().join(".claude/CLAUDE.md")).unwrap();
        assert!(claude_md_content.contains("## Infigraph — Primary Code Intelligence"));
        assert!(claude_md_content.contains("<!-- infigraph-primary-search -->"));

        let skill = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude/skills/infigraph-reindex/SKILL.md"))
            .expect("reindex skill artifact should be discovered from claude-code/config.toml");
        assert_eq!(skill.strategy, Strategy::Overwrite);
        apply_resolved_artifact(skill, home_dir.path(), mcp_path).unwrap();
        let skill_content = std::fs::read_to_string(
            home_dir.path().join(".claude/skills/infigraph-reindex/SKILL.md"),
        )
        .unwrap();
        assert!(skill_content.starts_with("---\nname: infigraph-reindex"));
        assert!(skill_content.contains("mcp__infigraph__index_project"));
    }

    #[test]
    fn bundled_settings_json_multi_event_test() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let settings = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".claude/settings.json"))
            .expect("settings.json fragment should be discovered");
        assert_eq!(settings.strategy, Strategy::JsonDeepMerge);

        apply_resolved_artifact(settings, home_dir.path(), mcp_path).unwrap();
        apply_resolved_artifact(settings, home_dir.path(), mcp_path).unwrap(); // reapply: must not duplicate

        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        for (event, expected_count) in [
            ("PreToolUse", 1),
            ("PostToolUse", 4),
            ("UserPromptSubmit", 3),
            ("SessionStart", 1),
            ("SessionEnd", 1),
            ("PreCompact", 1),
        ] {
            let arr = written["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("{event} should be an array"));
            assert_eq!(
                arr.len(),
                expected_count,
                "{event} should have exactly {expected_count} entries after reapply, got {arr:?}"
            );
        }

        let enforce_command = written["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(enforce_command.contains("infigraph-enforce.sh"));
    }

    #[test]
    fn bundled_hook_scripts_have_expected_content() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        for (relative, expected_substring) in [
            (".claude/hooks/infigraph-enforce.sh", "deny-by-default"),
            (".claude/hooks/infigraph-edit-tracker.sh", "recent_edits.log"),
            (".claude/hooks/infigraph-session-save.sh", "save_session"),
            (".claude/hooks/infigraph-session-reset.sh", "save_session"),
            (".claude/hooks/infigraph-session-start.sh", "inject_session_summary"),
            (".claude/hooks/infigraph-session-end-save.sh", "unsaved-transcript"),
            (".claude/hooks/infigraph-clear-suggest.sh", "save session and type"),
            (".claude/hooks/infigraph-clear-guard.sh", "Session not saved"),
            (".claude/hooks/infigraph-test-context-sentinel.sh", "generate_test_context"),
            (".claude/hooks/infigraph-search-fallback-sentinel.sh", "search-fallback-allowed"),
        ] {
            let artifact = artifacts
                .iter()
                .find(|a| a.target_relative_path.as_deref() == Some(relative))
                .unwrap_or_else(|| panic!("{relative} should be discovered"));
            assert_eq!(artifact.strategy, Strategy::Overwrite);
            apply_resolved_artifact(artifact, home_dir.path(), mcp_path).unwrap();
            let content = std::fs::read_to_string(home_dir.path().join(relative)).unwrap();
            assert!(
                content.contains(expected_substring),
                "{relative} should contain {expected_substring:?}"
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::integration_tests::bundled_claude -- --nocapture` and `cargo test -p infigraph-cli artifacts::integration_tests::bundled_settings -- --nocapture` and `cargo test -p infigraph-cli artifacts::integration_tests::bundled_hook -- --nocapture`
Expected: all fail (`.expect`/`.unwrap_or_else` panics) — none of the bundled files exist yet.

- [ ] **Step 3: Copy the 10 hook scripts verbatim from `hooks.rs`**

Each destination file's content is the named constant's raw-string body from `crates/infigraph-cli/src/hooks.rs` — everything from the line after the constant's `= r#"` (or `= r##"` for the two using `##` delimiters, since their content itself contains a `#`) through the line before the matching closing `"#;`/`"##;`, copied byte-for-byte (no Rust escaping to undo — these are raw strings). Strip only the opening/closing raw-string delimiter lines themselves.

| Destination | Source constant | Source lines (content only, delimiters excluded) |
|---|---|---|
| `claude-code/.claude/hooks/infigraph-enforce.sh` | `ENFORCE_HOOK_SCRIPT` | `hooks.rs:4-129` |
| `claude-code/.claude/hooks/infigraph-session-save.sh` | `SESSION_SAVE_HOOK_SCRIPT` | `hooks.rs:132-196` |
| `claude-code/.claude/hooks/infigraph-session-reset.sh` | `SESSION_RESET_HOOK_SCRIPT` | `hooks.rs:199-218` |
| `claude-code/.claude/hooks/infigraph-session-start.sh` | `SESSION_START_HOOK_SCRIPT` | `hooks.rs:221-347` |
| `claude-code/.claude/hooks/infigraph-session-end-save.sh` | `SESSION_END_SAVE_HOOK_SCRIPT` | `hooks.rs:350-413` |
| `claude-code/.claude/hooks/infigraph-clear-suggest.sh` | `CLEAR_SUGGEST_HOOK_SCRIPT` | `hooks.rs:416-439` |
| `claude-code/.claude/hooks/infigraph-clear-guard.sh` | `CLEAR_GUARD_HOOK_SCRIPT` | `hooks.rs:442-469` |
| `claude-code/.claude/hooks/infigraph-test-context-sentinel.sh` | `TEST_CONTEXT_SENTINEL_HOOK_SCRIPT` | `hooks.rs:583-596` |
| `claude-code/.claude/hooks/infigraph-search-fallback-sentinel.sh` | `SEARCH_FALLBACK_SENTINEL_HOOK_SCRIPT` | `hooks.rs:599-620` |
| `claude-code/.claude/hooks/infigraph-edit-tracker.sh` | `EDIT_TRACKER_HOOK_SCRIPT` | `hooks.rs:623-648` |

For each row: `Read` the source line range from the current `crates/infigraph-cli/src/hooks.rs` (re-check the exact line numbers first — Tasks 15-21 don't touch `hooks.rs`, but confirm nothing shifted them since this table was written), then `Write` that exact text to the destination path. No behavioral changes — this is a pure copy.

- [ ] **Step 4: Create `.claude.json`**

Write `crates/infigraph-cli/resources/integrations/claude-code/.claude.json`:

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

- [ ] **Step 5: Create `settings.json`**

Write `crates/infigraph-cli/resources/integrations/claude-code/.claude/settings.json`. Every `command` path is `~`-prefixed (tilde-expanded by the shell at hook-execution time, so it's correct for any user regardless of their actual `$HOME` — no `{{...}}` templating needed here) and, critically, keeps the `infigraph-` filename prefix so the array-ownership substring marker (`Strategy::JsonDeepMerge`'s "ours if it contains \"infigraph\"" rule) matches every entry:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Grep|Glob|Bash|Read|Write|Edit|Agent",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-enforce.sh",
            "timeout": 5
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write|NotebookEdit",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-edit-tracker.sh",
            "timeout": 5,
            "async": true
          }
        ]
      },
      {
        "matcher": "mcp__infigraph__save_session",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-session-reset.sh",
            "timeout": 5,
            "async": true
          }
        ]
      },
      {
        "matcher": "mcp__infigraph__generate_test_context",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-test-context-sentinel.sh",
            "timeout": 5,
            "async": true
          }
        ]
      },
      {
        "matcher": "mcp__infigraph__search|mcp__infigraph__search_code|mcp__infigraph__search_symbols|mcp__infigraph__list_files",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-search-fallback-sentinel.sh",
            "timeout": 5,
            "async": true
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-session-save.sh",
            "timeout": 5
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-clear-suggest.sh",
            "timeout": 5
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-clear-guard.sh",
            "timeout": 5
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-session-start.sh",
            "timeout": 5
          }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-session-end-save.sh",
            "timeout": 10
          }
        ]
      }
    ],
    "PreCompact": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/infigraph-session-end-save.sh",
            "timeout": 10
          }
        ]
      }
    ]
  }
}
```

- [ ] **Step 6: Create `shared/agents.md`**

Write `crates/infigraph-cli/resources/integrations/shared/agents.md` — this is `install.rs`'s current `write_claude_md_instructions` body (its inline `format!` string, minus the `{marker}` line the engine now adds itself via `marker_delimited`'s `start`/`end`), with the "Reindex" line reworded to be tool-neutral now that it's a shared skill, not a Claude-Code-only slash command:

```markdown
## Infigraph — Primary Code Intelligence

Infigraph MCP is indexed. Use Infigraph tools FIRST for all code tasks. Fall back to grep/Read only if Infigraph returns nothing or for non-code files.

### Rules
1. Check `list_projects` before indexing — don't re-index
2. **`search`** for ALL code search — hybrid BM25+vector+grep in one call, auto-escalates
3. **`get_doc_context`** before editing any function — returns source+callers+callees in one call
4. **`trace_callers`** / **`find_all_references`** before refactoring — never grep for callers
5. **`trace_callees`** / **`transitive_impact`** for blast radius — never manually trace call chains
6. Read files directly only for non-code files (configs, docs, manifests) or Edit tool line-number context

### Workflows
- **Find code:** `search` → if need symbol detail: `get_code_snippet` or `symbol_context`
- **Before editing:** `get_doc_context`
- **Before refactoring:** `find_all_references` → `transitive_impact` → edit
- **Onboarding:** `index_project` → `get_architecture` → `get_stats`
- **Multi-repo:** `group_create` → `group_add` × N → `group_index` → `group_sync` → `group_link`

### Subagents — infigraph-indexed projects
Do NOT spawn these agent types for code tasks — they lack MCP access and will fall back to grep/glob:
- **Explore** → use `search`, `search_code`, `search_symbols` directly instead
- **Plan** → use `get_architecture`, `get_skeleton`, `get_stats` directly instead
- **code-reviewer** → use `get_doc_context`, `get_code_snippet`, `review` directly instead

For tasks requiring a subagent, use **general-purpose** — it has full MCP/infigraph access.

### Verbose tools — delegate to subagent
`get_architecture`, `transitive_impact`, `detect_dead_code`, `detect_clusters`, `detect_clones`, `export_graph`, `query_graph`, `trace_callers`/`trace_callees` (deep), `group_query`, `group_index`

> All other Infigraph tools are safe to call inline. Each tool description says what it replaces — check descriptions when unsure which tool to use.

**Reindex:** use the `infigraph-reindex` skill directly (`/infigraph-reindex [path]` in tools with slash-command support) — runs inline, not via subagent, to save tokens.

### Session Continuity — MANDATORY
- **On session start:** MUST call `get_latest_session` to resume prior context
- **After context compaction:** if you see "continued from a previous conversation" or a compaction summary, IMMEDIATELY call `save_session` with whatever context survived before doing anything else
- **MUST call `save_session` IMMEDIATELY (before responding to the user)** when ANY of these occur. No session-end signal exists — if you don't save now, context is lost forever:
  1. **Finding** — root cause identified, discovered a bug, learned how something works
  2. **Milestone** — bug fixed and verified, feature committed, test passing, build green
  3. **Decision** — chose an approach, ruled something out, changed strategy
  4. **Task done** — any pending task from a prior session is completed
  5. **Periodic** — if you have NOT called `save_session` in the last 5 exchanges with the user, call it NOW regardless of whether anything dramatic happened. This is a hard rule, not a suggestion.
- Do NOT defer saves ("I'll save later"). Do NOT batch them. Do NOT wait for user to ask.
- "Later" does not exist — context compaction or session end can happen at any moment.
- **Before `/clear`:** ALWAYS call `save_session` first — `/clear` wipes context and LM2 can only restore what was persisted. Unsaved reasoning, decisions, and in-flight work will be lost.
- Same-day saves merge: summary/pending_tasks overwrite, decisions append, files_touched union
- **Narrative dumps:** On every `save_session`, include `narrative` field with full session story — what was explored, found, reasoned, decided, and why. Chronological prose, not terse bullets. Written to `.infigraph/sessions/session_YYYY-MM-DD.md` and embedded for semantic search. On session start, if `get_latest_session` shows a narrative log path, read it when structured fields aren't enough context.

### Session Field Guide
- **decisions** — structured format: `Goal: X. Decision: Y. Why: Z. Invalidates-if: W.`
- **constraints** — things that failed: `Tried: X. Failed because: Y. Do not retry unless: Z.`
- **assumptions** — what current approach depends on: `Assumes: X. If X changes: Y.`
- **blockers** — stuck items needing human input or external dependency
- **narrative** — full session story: explorations, findings, reasoning, code changes, decisions in chronological order. Write as prose, not structured fields.
```

- [ ] **Step 7: Create the shared reindex skill**

Write `crates/infigraph-cli/resources/integrations/shared/skills/infigraph-reindex/SKILL.md`:

```markdown
---
name: infigraph-reindex
description: Reindex the current project directly, without spawning a subagent, to save tokens. Use when the user asks to reindex, re-index, or refresh the infigraph index.
---

# Infigraph Reindex

Reindex the project directly (no subagent — saves tokens).

## Usage

Invoke with an optional path argument (`/infigraph-reindex [path]` in tools with slash-command support). If omitted, uses the current working directory.

## Instructions

1. Determine project path: use the argument provided, or fall back to the current working directory.
2. Load the tool schema: `ToolSearch("select:mcp__infigraph__index_project")`
3. Call `mcp__infigraph__index_project` with that path directly (do NOT spawn an Agent).
4. Report back in this exact format (nothing else):

```
Reindexed: <path>
Files: <N> | Symbols: <N> | Calls: <N> resolved / <N> unresolved
Languages: <comma-separated list with file counts>
```

If indexing fails, report the error verbatim. Do not attempt fixes.
```

- [ ] **Step 8: Create `claude-code/config.toml`**

Write `crates/infigraph-cli/resources/integrations/claude-code/config.toml`:

```toml
label = "Claude Code"

[[artifact]]
path = ".claude/CLAUDE.md"
strategy = "marker_delimited"
start = "<!-- infigraph-primary-search -->"
end = "<!-- /infigraph-primary-search -->"
content_file = "../shared/agents.md"

[[artifact]]
path = ".claude/skills/infigraph-reindex/SKILL.md"
strategy = "overwrite"
content_file = "../shared/skills/infigraph-reindex/SKILL.md"
```

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::integration_tests -- --nocapture`
Expected: every test in `integration_tests` passes, including all 4 new ones from this task and all 5 from Task 13.

- [ ] **Step 10: Commit**

```bash
git add crates/infigraph-cli/resources/integrations/claude-code crates/infigraph-cli/resources/integrations/shared crates/infigraph-cli/src/artifacts/mod.rs
git commit -m "feat(cli): bundle Claude Code hooks, settings.json, CLAUDE.md, and the shared reindex skill"
```

---

## Task 15: Cursor and Windsurf (shared rules content, convention-based MCP)

**Files:**
- Create: `crates/infigraph-cli/resources/integrations/cursor/config.toml`
- Create: `crates/infigraph-cli/resources/integrations/cursor/.cursor/mcp.json`
- Create: `crates/infigraph-cli/resources/integrations/windsurf/config.toml`
- Create: `crates/infigraph-cli/resources/integrations/windsurf/.codeium/windsurf/mcp_config.json`
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (fixture tests)

**Interfaces:**
- Consumes: everything from Tasks 1-14, specifically `shared/agents.md` (Task 14).

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod integration_tests { ... }` block in `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
    #[test]
    fn bundled_cursor_rules_and_mcp_apply_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".cursor/rules/infigraph.mdc"))
            .expect("cursor rules artifact should be discovered from cursor/config.toml");
        assert_eq!(rules.strategy, Strategy::Overwrite);
        apply_resolved_artifact(rules, home_dir.path(), mcp_path).unwrap();
        let rules_content =
            std::fs::read_to_string(home_dir.path().join(".cursor/rules/infigraph.mdc")).unwrap();
        assert!(rules_content.contains("## Infigraph — Primary Code Intelligence"));

        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".cursor/mcp.json"))
            .expect("cursor mcp.json fragment should be discovered");
        assert_eq!(mcp.strategy, Strategy::JsonDeepMerge);
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        let mcp_written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".cursor/mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mcp_written["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn bundled_windsurf_rules_and_mcp_apply_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".windsurf/rules/infigraph.md"))
            .expect("windsurf rules artifact should be discovered from windsurf/config.toml");
        assert_eq!(rules.strategy, Strategy::Overwrite);
        apply_resolved_artifact(rules, home_dir.path(), mcp_path).unwrap();
        let rules_content =
            std::fs::read_to_string(home_dir.path().join(".windsurf/rules/infigraph.md")).unwrap();
        assert!(rules_content.contains("## Infigraph — Primary Code Intelligence"));

        let mcp = artifacts
            .iter()
            .find(|a| {
                a.target_relative_path.as_deref() == Some(".codeium/windsurf/mcp_config.json")
            })
            .expect("windsurf mcp_config.json fragment should be discovered");
        assert_eq!(mcp.strategy, Strategy::JsonDeepMerge);
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        let mcp_written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".codeium/windsurf/mcp_config.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(mcp_written["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn shared_agents_md_override_changes_both_cursor_and_windsurf_output() {
        let user_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(user_dir.path().join("shared")).unwrap();
        std::fs::write(
            user_dir.path().join("shared/agents.md"),
            "## Overridden instructions",
        )
        .unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let cursor_rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".cursor/rules/infigraph.mdc"))
            .unwrap();
        assert_eq!(
            String::from_utf8(cursor_rules.content.clone().unwrap()).unwrap(),
            "## Overridden instructions"
        );

        let windsurf_rules = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".windsurf/rules/infigraph.md"))
            .unwrap();
        assert_eq!(
            String::from_utf8(windsurf_rules.content.clone().unwrap()).unwrap(),
            "## Overridden instructions"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::integration_tests::bundled_cursor -- --nocapture` and the `bundled_windsurf`/`shared_agents_md_override` variants
Expected: all fail — none of the bundled files exist yet.

- [ ] **Step 3: Create Cursor's files**

Write `crates/infigraph-cli/resources/integrations/cursor/config.toml`:

```toml
label = "Cursor"

[[artifact]]
path = ".cursor/rules/infigraph.mdc"
strategy = "overwrite"
content_file = "../shared/agents.md"
```

Write `crates/infigraph-cli/resources/integrations/cursor/.cursor/mcp.json`:

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

- [ ] **Step 4: Create Windsurf's files**

Write `crates/infigraph-cli/resources/integrations/windsurf/config.toml`:

```toml
label = "Windsurf"

[[artifact]]
path = ".windsurf/rules/infigraph.md"
strategy = "overwrite"
content_file = "../shared/agents.md"
```

Write `crates/infigraph-cli/resources/integrations/windsurf/.codeium/windsurf/mcp_config.json`:

```json
{"mcpServers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::integration_tests -- --nocapture`
Expected: every test passes, including the 3 new ones.

- [ ] **Step 6: Commit**

```bash
git add crates/infigraph-cli/resources/integrations/cursor crates/infigraph-cli/resources/integrations/windsurf crates/infigraph-cli/src/artifacts/mod.rs
git commit -m "feat(cli): bundle Cursor and Windsurf, both pulling shared/agents.md as rules"
```

---

## Task 16: Codex (TOML section MCP registration + shared reindex skill)

**Files:**
- Create: `crates/infigraph-cli/resources/integrations/codex/config.toml`
- Create: `crates/infigraph-cli/resources/integrations/codex/mcp-section.toml`
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (fixture tests)

**Interfaces:**
- Consumes: everything from Tasks 1-14, specifically `toml_section` (Task 6/12) and `shared/skills/infigraph-reindex/SKILL.md` (Task 14).

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod integration_tests { ... }` block in `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
    #[test]
    fn bundled_codex_mcp_and_skill_apply_correctly() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();

        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".codex/config.toml"))
            .expect("codex toml_section MCP artifact should be discovered");
        assert_eq!(mcp.strategy, Strategy::TomlSection);
        assert_eq!(
            mcp.key_path,
            Some(vec!["mcp_servers".to_string(), "infigraph".to_string()])
        );
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        let toml_content =
            std::fs::read_to_string(home_dir.path().join(".codex/config.toml")).unwrap();
        assert!(toml_content.contains("[mcp_servers.infigraph]"));
        assert!(toml_content.contains(mcp_path));

        let skill = artifacts
            .iter()
            .find(|a| {
                a.target_relative_path.as_deref() == Some(".codex/skills/infigraph-reindex/SKILL.md")
            })
            .expect("codex reindex skill artifact should be discovered");
        assert_eq!(skill.strategy, Strategy::Overwrite);
        apply_resolved_artifact(skill, home_dir.path(), mcp_path).unwrap();
        let skill_content = std::fs::read_to_string(
            home_dir.path().join(".codex/skills/infigraph-reindex/SKILL.md"),
        )
        .unwrap();
        assert!(skill_content.starts_with("---\nname: infigraph-reindex"));
    }

    #[test]
    fn bundled_codex_toml_reapply_does_not_duplicate_section() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let mcp = artifacts
            .iter()
            .find(|a| a.target_relative_path.as_deref() == Some(".codex/config.toml"))
            .unwrap();

        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();
        apply_resolved_artifact(mcp, home_dir.path(), mcp_path).unwrap();

        let toml_content =
            std::fs::read_to_string(home_dir.path().join(".codex/config.toml")).unwrap();
        assert_eq!(toml_content.matches("[mcp_servers.infigraph]").count(), 1);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::integration_tests::bundled_codex -- --nocapture`
Expected: both fail — the bundled files don't exist yet.

- [ ] **Step 3: Create Codex's files**

Write `crates/infigraph-cli/resources/integrations/codex/config.toml`:

```toml
label = "Codex"

[[artifact]]
path = ".codex/config.toml"
strategy = "toml_section"
key_path = ["mcp_servers", "infigraph"]
content_file = "mcp-section.toml"

[[artifact]]
path = ".codex/skills/infigraph-reindex/SKILL.md"
strategy = "overwrite"
content_file = "../shared/skills/infigraph-reindex/SKILL.md"
```

Write `crates/infigraph-cli/resources/integrations/codex/mcp-section.toml`:

```toml
command = "{{mcp_path}}"
args = ["--mcp"]
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::integration_tests -- --nocapture`
Expected: every test passes, including the 2 new ones.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/resources/integrations/codex crates/infigraph-cli/src/artifacts/mod.rs
git commit -m "feat(cli): bundle Codex's toml_section MCP registration and reindex skill"
```

---

## Task 17: VS Code and Zed (resolver-driven path/content)

VS Code's resolver only determines the destination *path* (its content is a static local fragment, `strategy = "json_deep_merge"`). Zed's resolver determines both path and content (`strategy = "json_key_path"`, since the returned value has no enclosing key structure of its own to self-describe a merge target the way a `content_file` fragment does).

**Files:**
- Create: `crates/infigraph-cli/resources/integrations/vscode/config.toml`
- Create: `crates/infigraph-cli/resources/integrations/vscode/mcp-fragment.json`
- Create: `crates/infigraph-cli/resources/integrations/vscode/resolve-vscode-path.py`
- Create: `crates/infigraph-cli/resources/integrations/zed/config.toml`
- Create: `crates/infigraph-cli/resources/integrations/zed/resolve-zed-path.py`
- Modify: `crates/infigraph-cli/src/artifacts/mod.rs` (fixture tests)

**Interfaces:**
- Consumes: `resolver::run_resolver_from_script` (Task 12), `Strategy::JsonKeyPath`/`Strategy::JsonDeepMerge` with a resolver (Task 12).

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod integration_tests { ... }` block in `crates/infigraph-cli/src/artifacts/mod.rs`:

```rust
    #[test]
    fn bundled_vscode_resolver_resolves_path_and_uses_local_content() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let vscode = artifacts
            .iter()
            .find(|a| a.integration_label == "VS Code")
            .expect("VS Code resolver artifact should be discovered");
        assert_eq!(vscode.strategy, Strategy::JsonDeepMerge);
        assert!(vscode.target_relative_path.is_none());
        assert!(vscode.resolver.is_some());

        let outcome = apply_resolved_artifact(vscode, home_dir.path(), mcp_path).unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));

        // The resolver branches on OS -- assert against whichever path it
        // actually resolved to for the OS running this test.
        let expected_suffix = match std::env::consts::OS {
            "macos" => "Library/Application Support/Code/User/mcp.json",
            "linux" => ".config/Code/User/mcp.json",
            "windows" => "AppData/Roaming/Code/User/mcp.json",
            other => panic!("unhandled test OS {other}"),
        };
        let path = home_dir.path().join(expected_suffix);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["servers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn bundled_zed_resolver_resolves_path_and_content() {
        let user_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let artifacts = discover_artifacts(BUNDLED_INTEGRATIONS, user_dir.path(), mcp_path).unwrap();
        let zed = artifacts
            .iter()
            .find(|a| a.integration_label == "Zed")
            .expect("Zed resolver artifact should be discovered");
        assert_eq!(zed.strategy, Strategy::JsonKeyPath);
        assert_eq!(
            zed.key_path,
            Some(vec!["context_servers".to_string(), "infigraph".to_string()])
        );
        assert!(zed.target_relative_path.is_none());

        let outcome = apply_resolved_artifact(zed, home_dir.path(), mcp_path).unwrap();
        assert!(matches!(outcome, ApplyOutcome::Written));

        let expected_suffix = match std::env::consts::OS {
            "macos" => "Library/Application Support/Zed/settings.json",
            "linux" => ".config/zed/settings.json",
            "windows" => "AppData/Roaming/Zed/settings.json",
            other => panic!("unhandled test OS {other}"),
        };
        let path = home_dir.path().join(expected_suffix);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["context_servers"]["infigraph"]["command"], mcp_path);
        assert_eq!(written["context_servers"]["infigraph"]["args"][0], "--mcp");
    }
```

These two tests require `python3` on the test runner's `PATH`, consistent with several of this repo's existing hook scripts (`hooks.rs`'s `SESSION_START_HOOK_SCRIPT` already assumes it).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p infigraph-cli artifacts::integration_tests::bundled_vscode -- --nocapture` and `bundled_zed`
Expected: both fail (`.expect` panics) — the bundled files don't exist yet.

- [ ] **Step 3: Create VS Code's files**

Write `crates/infigraph-cli/resources/integrations/vscode/config.toml`:

```toml
label = "VS Code"

[[artifact]]
strategy = "json_deep_merge"
resolver = ["./resolve-vscode-path.py"]
content_file = "mcp-fragment.json"
```

Write `crates/infigraph-cli/resources/integrations/vscode/mcp-fragment.json`:

```json
{"servers":{"infigraph":{"command":"{{mcp_path}}","args":["--mcp"]}}}
```

Write `crates/infigraph-cli/resources/integrations/vscode/resolve-vscode-path.py` (executable):

```python
#!/usr/bin/env python3
"""Resolves VS Code's user-level mcp.json path for the default profile.

stdin:  {"mcp_path": "...", "os": "macos"|"linux"|"windows", "home": "..."}
stdout: {"status": "ok", "data": {"path": "..."}} or {"status": "skip", "message": "..."}
"""
import json
import sys

data = json.load(sys.stdin)
os_name = data["os"]
home = data["home"]

paths = {
    "macos": f"{home}/Library/Application Support/Code/User/mcp.json",
    "linux": f"{home}/.config/Code/User/mcp.json",
    "windows": f"{home}/AppData/Roaming/Code/User/mcp.json",
}

path = paths.get(os_name)
if path is None:
    print(json.dumps({"status": "skip", "message": f"unsupported OS: {os_name}"}))
else:
    print(json.dumps({"status": "ok", "data": {"path": path}}))
```

- [ ] **Step 4: Create Zed's files**

Write `crates/infigraph-cli/resources/integrations/zed/config.toml`:

```toml
label = "Zed"

[[artifact]]
strategy = "json_key_path"
key_path = ["context_servers", "infigraph"]
resolver = ["./resolve-zed-path.py"]
```

Write `crates/infigraph-cli/resources/integrations/zed/resolve-zed-path.py` (executable):

```python
#!/usr/bin/env python3
"""Resolves Zed's settings.json path (default profile) and generates the
context_servers.infigraph fragment directly -- Zed's settings.json is a
general-purpose file with far more content than just this section, so unlike
VS Code there's no separate mirrored file to keep as a static local fragment.

stdin:  {"mcp_path": "...", "os": "macos"|"linux"|"windows", "home": "..."}
stdout: {"status": "ok", "data": {"path": "...", "content": {...}}} or {"status": "skip", "message": "..."}
"""
import json
import sys

data = json.load(sys.stdin)
os_name = data["os"]
home = data["home"]
mcp_path = data["mcp_path"]

paths = {
    "macos": f"{home}/Library/Application Support/Zed/settings.json",
    "linux": f"{home}/.config/zed/settings.json",
    "windows": f"{home}/AppData/Roaming/Zed/settings.json",
}

path = paths.get(os_name)
if path is None:
    print(json.dumps({"status": "skip", "message": f"unsupported OS: {os_name}"}))
else:
    content = {"command": mcp_path, "args": ["--mcp"], "env": {}}
    print(json.dumps({"status": "ok", "data": {"path": path, "content": content}}))
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli artifacts::integration_tests -- --nocapture`
Expected: every test passes, including the 2 new ones. On Windows CI, confirm the resolver scripts are still invoked correctly through `python3` (not `python`) — if the runner only has `python`, add a `#!/usr/bin/env python3` fallback note to the CI config rather than changing the script's shebang, since `python3` is the correct modern convention and matches the existing hook scripts.

- [ ] **Step 6: Commit**

```bash
git add crates/infigraph-cli/resources/integrations/vscode crates/infigraph-cli/resources/integrations/zed crates/infigraph-cli/src/artifacts/mod.rs
git commit -m "feat(cli): bundle VS Code and Zed resolver scripts for profile-dependent paths"
```

---

## Task 18: Rewire `cmd_install` onto the artifact engine

**Files:**
- Modify: `crates/infigraph-cli/src/install.rs`

**Interfaces:**
- Consumes: `artifacts::{discover_artifacts, apply_resolved_artifact, ApplyOutcome, BUNDLED_INTEGRATIONS}` (Tasks 1-17), `find_mcp_binary` (existing, unchanged), `install_models`/`install_claude_allowlist` (existing, unchanged per the design spec's "stays outside the artifact mechanism").
- Produces: `pub(crate) fn cmd_install() -> Result<()>` with the same external behavior/output shape users already expect (per-agent "Configured X" style reporting), now driven by the engine.

- [ ] **Step 1: Write the failing test**

Add to a new `#[cfg(test)] mod tests { ... }` block at the end of `crates/infigraph-cli/src/install.rs` (there isn't one yet — this is the first test in this file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_install_writes_every_convention_based_integration_under_a_fake_home() {
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        let report = run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();

        assert!(report.written.iter().any(|p| p == ".claude.json"));
        assert!(report.written.iter().any(|p| p == ".gemini/settings.json"));
        assert!(report.written.iter().any(|p| p == ".codex/config.toml"));
        assert!(report.skipped.is_empty(), "nothing should be skipped against an empty $HOME");

        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(claude_json["mcpServers"]["infigraph"]["command"], mcp_path);
    }

    #[test]
    fn cmd_install_is_idempotent() {
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();
        run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();

        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            claude_json["mcpServers"]["infigraph"]["args"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p infigraph-cli install::tests -- --nocapture`
Expected: compile error (`run_install`, `report.written`/`report.skipped` don't exist yet).

- [ ] **Step 3: Implement `run_install` and rewire `cmd_install` to call it**

In `crates/infigraph-cli/src/install.rs`, add near the top (below the existing `use` lines):

```rust
pub(crate) struct InstallReport {
    pub written: Vec<String>,
    pub skipped: Vec<(String, String)>, // (path, reason)
}
```

Replace the body of `cmd_install` (currently lines 42-102, from `pub(crate) fn cmd_install() -> Result<()> {` through its closing `Ok(())\n}`) with:

```rust
pub(crate) fn cmd_install() -> Result<()> {
    let mcp_path = find_mcp_binary()?;
    println!("Found infigraph-mcp at: {}", mcp_path.to_string_lossy());

    let home = dirs::home_dir().context("Could not determine home directory")?;

    let report = run_install(&mcp_path, &home)?;

    if report.written.is_empty() {
        println!("No agents were configured.");
    } else {
        for path in &report.written {
            println!("  Configured: {}", home.join(path).display());
        }
        let configured_labels = configured_integration_labels(&mcp_path, &home, &report)?;
        print_capabilities_summary(&configured_labels);
    }

    for (path, reason) in &report.skipped {
        eprintln!("  Skipped {}: {}", home.join(path).display(), reason);
    }

    // Copy model files to ~/.infigraph/models/ -- unchanged, not artifact-based.
    install_models(&mcp_path, &home)?;

    Ok(())
}

/// The actual artifact-engine install logic, factored out from `cmd_install`
/// so it's testable against a fake `$HOME` without touching the real one.
pub(crate) fn run_install(
    mcp_path: &Path,
    home: &Path,
) -> Result<InstallReport> {
    let mcp_path_str = mcp_path.to_string_lossy().to_string();
    let user_override_dir = home.join(".infigraph").join("integrations");

    let artifacts = crate::artifacts::discover_artifacts(
        crate::artifacts::BUNDLED_INTEGRATIONS,
        &user_override_dir,
        &mcp_path_str,
    )?;

    let mut report = InstallReport {
        written: Vec::new(),
        skipped: Vec::new(),
    };

    for artifact in &artifacts {
        let outcome = crate::artifacts::apply_resolved_artifact(artifact, home, &mcp_path_str)
            .with_context(|| {
                format!(
                    "applying {} artifact for {}",
                    artifact.integration_label,
                    artifact
                        .target_relative_path
                        .as_deref()
                        .unwrap_or("(resolver-determined path)")
                )
            })?;
        let label = artifact
            .target_relative_path
            .clone()
            .unwrap_or_else(|| format!("{} (resolver-determined)", artifact.integration_label));
        match outcome {
            crate::artifacts::ApplyOutcome::Written => report.written.push(label),
            crate::artifacts::ApplyOutcome::Skipped { reason, .. } => {
                report.skipped.push((label, reason))
            }
        }
    }

    write_claude_allowlist_and_hooks_extras(home)?;

    Ok(report)
}

/// Everything the artifact engine doesn't cover: the Claude Code permission
/// allowlist (a grant list, not "content deployed to a path" -- see the
/// design spec's "stays outside the artifact mechanism").
fn write_claude_allowlist_and_hooks_extras(home: &Path) -> Result<()> {
    crate::hooks::install_claude_allowlist(home)?;
    Ok(())
}

/// Derives the human-readable "Configured for: X, Y, Z" summary from which
/// integrations actually had at least one artifact written -- replaces the
/// old per-`AgentTarget` `configured.push(target.label)` bookkeeping now that
/// artifacts (not agent targets) are the unit of installation.
fn configured_integration_labels(
    mcp_path: &Path,
    home: &Path,
    report: &InstallReport,
) -> Result<Vec<String>> {
    let mcp_path_str = mcp_path.to_string_lossy().to_string();
    let user_override_dir = home.join(".infigraph").join("integrations");
    let artifacts = crate::artifacts::discover_artifacts(
        crate::artifacts::BUNDLED_INTEGRATIONS,
        &user_override_dir,
        &mcp_path_str,
    )?;

    let written_set: std::collections::HashSet<&str> =
        report.written.iter().map(|s| s.as_str()).collect();

    let mut labels: Vec<String> = artifacts
        .iter()
        .filter(|a| {
            let key = a
                .target_relative_path
                .as_deref()
                .map(|p| p.to_string())
                .unwrap_or_else(|| format!("{} (resolver-determined)", a.integration_label));
            written_set.contains(key.as_str())
        })
        .map(|a| a.integration_label.clone())
        .collect();
    labels.sort();
    labels.dedup();
    Ok(labels)
}
```

Update the top of the file: change `use crate::config_targets::{self, ConfigFormat, AGENT_TARGETS};` to remove that import entirely (nothing in the new `cmd_install` references `config_targets` anymore — it's deleted in Task 20) and add `use std::path::Path;` if not already present via the existing `use std::path::{Path, PathBuf};` line (it already is, at line 1 — no change needed there).

Also update `print_capabilities_summary`'s signature at the bottom of the file from `pub(crate) fn print_capabilities_summary(configured: &[&str])` to `pub(crate) fn print_capabilities_summary(configured: &[String])`, and its one call site inside it (`configured.join(", ")`) — `Vec<String>::join` and `&[&str]::join` both work identically here, so the function body itself needs no other change, only the parameter type.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli install:: -- --nocapture`
Expected: both new tests pass. `write_claude_md_instructions`/`write_editor_rules`/`write_reindex_command` still exist in the file at this point (Task 20 deletes them) but are no longer called by anything — this compiles fine, just with `dead_code` warnings, which is expected and temporary; `cargo fmt`/`clippy -D warnings` aren't run again until Task 20 Step 5, by which point they're gone.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/install.rs
git commit -m "feat(cli): rewire cmd_install onto the artifact engine"
```

---

## Task 19: Rewire `cmd_uninstall` onto the artifact engine

**Files:**
- Modify: `crates/infigraph-cli/src/install.rs`

**Interfaces:**
- Consumes: `artifacts::{discover_artifacts, remove_resolved_artifact, BUNDLED_INTEGRATIONS}` (Tasks 1-17).
- Produces: `pub(crate) fn cmd_uninstall() -> Result<()>`, same external behavior, engine-driven.

- [ ] **Step 1: Write the failing test**

Add to `crates/infigraph-cli/src/install.rs`'s `#[cfg(test)] mod tests { ... }` block (from Task 18):

```rust
    #[test]
    fn cmd_uninstall_removes_everything_cmd_install_wrote() {
        let home_dir = tempfile::tempdir().unwrap();
        let mcp_path = "/opt/infigraph/bin/infigraph-mcp";

        run_install(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();
        assert!(home_dir.path().join(".claude.json").exists());

        run_uninstall(&std::path::PathBuf::from(mcp_path), home_dir.path()).unwrap();

        let claude_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home_dir.path().join(".claude.json")).unwrap(),
        )
        .unwrap();
        assert!(claude_json["mcpServers"]["infigraph"].is_null());
        assert!(!home_dir.path().join(".claude/hooks/infigraph-enforce.sh").exists());

        let codex_toml =
            std::fs::read_to_string(home_dir.path().join(".codex/config.toml")).unwrap();
        assert!(!codex_toml.contains("[mcp_servers.infigraph]"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p infigraph-cli install::tests::cmd_uninstall -- --nocapture`
Expected: compile error (`run_uninstall` doesn't exist yet).

- [ ] **Step 3: Implement `run_uninstall` and rewire `cmd_uninstall`**

Replace the body of `cmd_uninstall` (currently lines 305-418, from `pub(crate) fn cmd_uninstall() -> Result<()> {` through its closing `Ok(())\n}`) with:

```rust
pub(crate) fn cmd_uninstall() -> Result<()> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let mcp_path = find_mcp_binary().unwrap_or_else(|_| PathBuf::from("infigraph-mcp"));

    let removed = run_uninstall(&mcp_path, &home)?;

    if removed.is_empty() {
        println!("No agents had infigraph configured.");
    } else {
        println!(
            "\nUninstalled infigraph MCP server artifacts: {}",
            removed.join(", ")
        );
    }

    // Remove hooks and Claude Code allowlist -- outside the artifact mechanism.
    crate::hooks::uninstall_hooks(&home)?;
    crate::hooks::uninstall_claude_allowlist(&home)?;

    // Remove binaries from ~/.local/bin/
    for bin in &["infigraph", "infigraph-mcp"] {
        let bin_path = home.join(".local").join("bin").join(bin);
        if bin_path.exists() {
            std::fs::remove_file(&bin_path)?;
            println!("  Removed binary: {}", bin_path.display());
        }
    }

    // Remove project-level CLAUDE.md managed blocks from all registered projects
    if let Ok(registry) = infigraph_core::multi::Registry::load() {
        for (name, entry) in &registry.repos {
            match infigraph_core::claude_md::remove_project_claude_md(&entry.path) {
                Ok(true) => println!("  Removed CLAUDE.md block from {}", name),
                Ok(false) => {}
                Err(e) => eprintln!("  warning: failed to clean CLAUDE.md for {}: {e}", name),
            }
        }
    }

    // Remove model cache ~/.infigraph/
    let model_cache = home.join(".infigraph");
    if model_cache.exists() {
        std::fs::remove_dir_all(&model_cache)?;
        println!("  Removed model cache: {}", model_cache.display());
    }

    Ok(())
}

/// The actual artifact-engine uninstall logic, factored out from
/// `cmd_uninstall` so it's testable against a fake `$HOME`. Returns the list
/// of artifact labels that were actually removed (mirrors `run_install`'s
/// `InstallReport.written` shape, one label per artifact whose `remove_*`
/// call reported `true`).
pub(crate) fn run_uninstall(mcp_path: &Path, home: &Path) -> Result<Vec<String>> {
    let mcp_path_str = mcp_path.to_string_lossy().to_string();
    let user_override_dir = home.join(".infigraph").join("integrations");

    let artifacts = crate::artifacts::discover_artifacts(
        crate::artifacts::BUNDLED_INTEGRATIONS,
        &user_override_dir,
        &mcp_path_str,
    )?;

    let mut removed = Vec::new();
    for artifact in &artifacts {
        let was_removed =
            crate::artifacts::remove_resolved_artifact(artifact, home, &mcp_path_str)
                .with_context(|| format!("removing {} artifact", artifact.integration_label))?;
        if was_removed {
            removed.push(artifact.integration_label.clone());
        }
    }
    removed.sort();
    removed.dedup();
    Ok(removed)
}
```

(`remove_resolved_artifact` is `pub(crate)` from Task 11/12 in `crates/infigraph-cli/src/artifacts/mod.rs` — confirm it's still exported there; no change needed if so.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p infigraph-cli install:: -- --nocapture`
Expected: all `install::tests` pass, including the new uninstall test. Same as Task 18 Step 4 — this compiles fine with `dead_code` warnings for the not-yet-deleted old functions, which Task 20 removes.

- [ ] **Step 5: Commit**

```bash
git add crates/infigraph-cli/src/install.rs
git commit -m "feat(cli): rewire cmd_uninstall onto the artifact engine"
```

---

## Task 20: Delete the superseded code

**Files:**
- Delete: `crates/infigraph-cli/src/config_targets.rs`
- Modify: `crates/infigraph-cli/src/main.rs` (remove `mod config_targets;`)
- Modify: `crates/infigraph-cli/src/hooks.rs` (remove the 10 hook-script constants and their `install_*_hook` functions, plus `uninstall_hooks`' now-hardcoded hook-file list — see Step 2)
- Modify: `crates/infigraph-cli/src/install.rs` (remove `write_claude_md_instructions`, `write_editor_rules`, `write_reindex_command`, and their call sites — already unused after Tasks 18-19's rewiring)

This task removes dead code only — no behavior change beyond what Tasks 18-19 already introduced. It's a separate task (not folded into 18/19) so a reviewer can verify "nothing here is still referenced" as its own gate, per this plan's Task Right-Sizing.

**Interfaces:**
- Consumes: nothing new.
- Produces: nothing new — pure deletion.

- [ ] **Step 1: Delete `config_targets.rs` and its module declaration**

```bash
git rm crates/infigraph-cli/src/config_targets.rs
```

In `crates/infigraph-cli/src/main.rs`, delete the line `mod config_targets;`.

- [ ] **Step 2: Remove the superseded hook functions and constants from `hooks.rs`**

In `crates/infigraph-cli/src/hooks.rs`, delete:
- The 10 script constants: `ENFORCE_HOOK_SCRIPT`, `SESSION_SAVE_HOOK_SCRIPT`, `SESSION_RESET_HOOK_SCRIPT`, `SESSION_START_HOOK_SCRIPT`, `SESSION_END_SAVE_HOOK_SCRIPT`, `CLEAR_SUGGEST_HOOK_SCRIPT`, `CLEAR_GUARD_HOOK_SCRIPT`, `TEST_CONTEXT_SENTINEL_HOOK_SCRIPT`, `SEARCH_FALLBACK_SENTINEL_HOOK_SCRIPT`, `EDIT_TRACKER_HOOK_SCRIPT` (their content now lives at `crates/infigraph-cli/resources/integrations/claude-code/.claude/hooks/infigraph-*.sh`, copied verbatim in Task 14, so nothing is lost).
- The corresponding install functions: `install_enforcement_hook`, `install_edit_tracker_hook`, `install_session_save_hook`, `install_clear_suggest_hook`, `install_clear_guard_hook`, `install_session_end_hook`, `install_test_context_sentinel_hook`, `install_search_fallback_sentinel_hook` (superseded by the `settings.json` convention-based artifact from Task 14).
- Every `#[cfg(test)] mod tests { ... }` test in `hooks.rs` that references any of the deleted constants/functions (`install_enforcement_hook_creates_file_and_settings`, `install_enforcement_hook_idempotent`, `install_edit_tracker_hook_creates_file`, `install_test_context_sentinel`, `install_search_fallback_sentinel`, `install_session_hooks`, `install_session_end_hook_creates_file`, `install_clear_suggest_hook_creates_file`, `install_clear_guard_hook_creates_file`, `enforce_script_covers_all_tool_cases`, `search_fallback_sentinel_covers_all_search_tools`, `session_start_resets_sentinels_on_clear`) — their coverage is superseded by Task 14's `bundled_settings_json_multi_event_test` and `bundled_hook_scripts_have_expected_content`.

Keep `uninstall_hooks`, but simplify its hardcoded `hook_file` list — it no longer needs a fixed list of exactly 10 filenames, since the artifact engine's uninstall path (Task 19) already handles removing hook script *files* via each hook's convention-based `overwrite` artifact's `remove_overwrite` (deletes the file) and the `settings.json` entries via `remove_json_deep_merge`. Delete the `for hook_file in &[...]` loop's file-removal half entirely (lines that currently do `std::fs::remove_file(&hook_path)` for each named file); keep the rest of the function (the `settings.json` cleanup loop over `PreToolUse`/`UserPromptSubmit`/etc., which still legitimately handles hooks a *user* hand-added outside the artifact system, e.g. via `~/.infigraph/integrations/` overrides that don't map to a known convention file). Also keep `allowed_tools`, `install_claude_allowlist`, and `uninstall_claude_allowlist` — untouched, per the design spec ("stays outside the artifact mechanism").

- [ ] **Step 3: Remove the superseded docs/rules/reindex functions from `install.rs`**

In `crates/infigraph-cli/src/install.rs`, delete the three now-unreferenced functions: `write_claude_md_instructions`, `write_editor_rules`, `write_reindex_command` (Tasks 18-19's rewritten `cmd_install`/`cmd_uninstall` never call them).

- [ ] **Step 4: Run the full crate test suite**

Run: `cargo test -p infigraph-cli`
Expected: all tests pass — this is the first point at which Tasks 18-19's `install::tests` module actually compiles cleanly (no leftover references to the deleted `config_targets`/hook functions).

- [ ] **Step 5: Run fmt and clippy**

Run: `cargo fmt --all -- --check`
Expected: no diff.

Run: `cargo clippy -p infigraph-cli --all-targets -- -D warnings`
Expected: no warnings — in particular, no unused-import or dead-code warnings for anything this task was supposed to delete.

- [ ] **Step 6: Commit**

```bash
git add -A crates/infigraph-cli
git commit -m "chore(cli): delete config_targets.rs and the hardcoded hook/docs functions it replaces"
```

---

## Task 21: Full-workspace verification

**Files:**
- None (verification only).

- [ ] **Step 1: Full workspace build**

Run: `cargo build --release -p infigraph-cli -p infigraph-mcp`
Expected: builds cleanly.

- [ ] **Step 2: Full workspace test suite**

Run: `cargo test --all`
Expected: all tests pass across every crate (per this repo's CLAUDE.md, `--all` matters here — this repo has real process-level integration tests under `infigraph-mcp/tests/`, not just unit tests). If disk space is tight, batch per-crate instead (`cargo test -p infigraph-cli`, `cargo test -p infigraph-mcp`, etc.) per this repo's known disk-constrained workflow.

- [ ] **Step 3: fmt and clippy across the whole workspace**

Run: `cargo fmt --all -- --check`
Expected: no diff.

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings anywhere in the workspace, not just `infigraph-cli`.

- [ ] **Step 4: Manual smoke test against a real (but disposable) `$HOME`**

```bash
export SMOKE_HOME=$(mktemp -d)
HOME="$SMOKE_HOME" cargo run -p infigraph-cli --bin infigraph -- install
ls -la "$SMOKE_HOME/.claude.json" "$SMOKE_HOME/.claude/settings.json" "$SMOKE_HOME/.claude/hooks/" "$SMOKE_HOME/.codex/config.toml"
cat "$SMOKE_HOME/.claude.json"
HOME="$SMOKE_HOME" cargo run -p infigraph-cli --bin infigraph -- uninstall
ls "$SMOKE_HOME/.claude.json" 2>&1 || echo "correctly removed or emptied"
rm -rf "$SMOKE_HOME"
```

Expected: `install` reports every integration configured with no unexpected skips (an empty `$HOME` has no pre-existing JSONC/comment files to trip the parse-safety bail path); the listed files exist with real, substituted `infigraph-mcp` paths (not literal `{{mcp_path}}`); `uninstall` removes/empties them.

- [ ] **Step 5: No commit needed**

This task is pure verification — if any step fails, fix the specific regression in the task that introduced it and re-run this task's steps from the top; nothing new to commit here once everything is green.

---

## Self-Review Notes (for the implementer to re-check before declaring this plan done)

- **Spec coverage:** every strategy in the design spec (`json_deep_merge`, `overwrite`, `marker_delimited`, `toml_section`, `json_key_path`/resolver) has an `apply_*`/`remove_*` pair and dedicated tests (Tasks 4-8). Two-tier discovery with bundled+user-override and manifest-vs-convention classification is covered (Task 9). `InstallStep` groundwork is covered (Task 10). Template substitution — a gap not explicitly detailed in the design spec but required for any of this to produce a real, installable MCP entry — is covered (Task 3). Resolver-driven artifacts genuinely resolving to a real, dynamically-computed path and content is covered by Task 12. All 13 integrations get real bundled content with fixture tests (Tasks 13-17), matching the design spec's per-agent research table and layout. `cmd_install`/`cmd_uninstall` are rewired onto the engine (Tasks 18-19) and the superseded code is deleted (Task 20). The reindex-as-shared-skill conversion ships as part of Task 14/16 (Claude Code, Codex) rather than a separate follow-on, per the "fold it into the migration, skip ever building a `commands/` convention path" decision.
- **Known deliberate deviations from the design spec's literal text**, each worth a one-line mention in the eventual PR description alongside the other "deliberate behavior changes":
  1. The array-ownership rule gains an exact-match fallback beyond the spec's substring-only wording (proven necessary by the `args: ["--mcp"]` duplication-bug regression test in Task 4 Step 1).
  2. `ArtifactEntry.path` is optional, not required — the spec's own Zed example manifest omits `path` entirely; Task 12 makes the schema match the spec's own example.
  3. The spec's Claude Code directory-layout diagram was corrected (this session, before Task 14 was written) to nest `settings.json`/`hooks/` under `.claude/`, matching every other integration's own diagram entry, and hook script filenames keep the `infigraph-` prefix so the array-ownership marker still matches them.
  4. `content_file` is allowed on a resolver-driven artifact with no static `path` (needed for VS Code, whose resolver determines only the path); template-format inference for a manifest artifact's `content_file` is strategy-based, not path-based, to support this.
- **Real behavior changes from the current shipped tool** (distinct from spec deviations above — these are user-visible differences from what `infigraph install`/`uninstall` do today, also worth their own PR-description callout): per-agent path/shape fixes for issue #29 (Windsurf, Kiro, GitHub Copilot CLI, OpenCode, VS Code, Zed — see the spec's research table); universal hook matcher self-healing and the dropped edit-tracker merge-into-existing-entry special case (both already documented in the spec's "Deliberate behavior changes" section); Cursor and Windsurf's rules now get the fuller instructional text CLAUDE.md already had (Subagents guidance, Verbose tools guidance, the reindex mention) instead of the shorter text `agent::infigraph_instructions()` previously gave them — that function and its callers (`cmd_init`'s project-level `AGENTS.md`/`GEMINI.md`/etc. writers) are untouched, out of scope, and keep using their own separate text.
