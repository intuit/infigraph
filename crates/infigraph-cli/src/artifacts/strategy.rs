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
                    let child_is_empty = child.as_object().map(|m| m.is_empty()).unwrap_or(false);
                    if child_changed && child_is_empty {
                        target_map.remove(key);
                    }
                }
            }
            serde_json::Value::Array(fragment_entries) => {
                if let Some(existing) = target_map.get(key).and_then(|v| v.as_array()) {
                    let before = existing.len();
                    let filtered: Vec<serde_json::Value> = existing
                        .iter()
                        .filter(|entry| !is_owned_array_entry(entry, fragment_entries))
                        .cloned()
                        .collect();
                    removed_any |= filtered.len() < before;
                    // An owned array that's now empty must remove its own key too,
                    // so a fully-owned object (e.g. mcpServers.infigraph, whose only
                    // fields are a scalar "command" and an array "args") disappears
                    // entirely rather than being left behind as `{"args": []}`,
                    // which would otherwise block the parent's own is-empty check.
                    if filtered.is_empty() {
                        target_map.remove(key);
                    } else {
                        target_map.insert(key.clone(), serde_json::Value::Array(filtered));
                    }
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

pub(crate) fn apply_json_deep_merge(
    target_path: &Path,
    fragment_content: &str,
) -> Result<ApplyOutcome> {
    let fragment: serde_json::Value = serde_json::from_str(fragment_content).context(
        "bundled/user fragment is not valid JSON (this is an infigraph bug, please report)",
    )?;

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
    let fragment: serde_json::Value = serde_json::from_str(fragment_content).context(
        "bundled/user fragment is not valid JSON (this is an infigraph bug, please report)",
    )?;
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

    let start_pos = existing.find(start);
    let end_pos = start_pos.and_then(|sp| existing[sp..].find(end).map(|p| sp + p + end.len()));

    let new_content = match (start_pos, end_pos) {
        (Some(start_pos), Some(end_pos)) => {
            format!(
                "{}{}{}",
                &existing[..start_pos],
                block,
                &existing[end_pos..]
            )
        }
        (Some(_), None) => {
            // Start marker present but end marker missing or damaged (e.g. a
            // user hand-edited the file and deleted/corrupted it). Guessing
            // "the block ends at EOF" would silently destroy every byte of
            // real user content after the start marker -- refuse instead,
            // same as the JSON-parse-failure bail path.
            return Ok(ApplyOutcome::Skipped {
                reason: format!(
                    "{} has the start marker \"{start}\" but not a matching end marker \"{end}\" -- refusing to guess where the managed block ends",
                    target_path.display()
                ),
                manual_snippet: block,
            });
        }
        (None, _) => {
            let sep = if existing.is_empty() || existing.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{existing}{sep}{block}\n")
        }
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
    // Same refusal as apply_marker_delimited: a missing/damaged end marker
    // must not be treated as "the block extends to EOF" -- that would delete
    // real trailing user content instead of just the managed block.
    let Some(end_pos) = existing[start_pos..]
        .find(end)
        .map(|p| start_pos + p + end.len())
    else {
        return Ok(false);
    };

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
        assert_eq!(
            v["mcpServers"]["infigraph"]["command"],
            "/bin/infigraph-mcp"
        );
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
        assert_eq!(
            v["mcpServers"]["infigraph"]["command"],
            "/bin/infigraph-mcp"
        );
    }

    #[test]
    fn json_deep_merge_reapply_does_not_duplicate_scalar_owned_array() {
        // Regression test for the substring-only ownership rule's duplication
        // bug: "args": ["--mcp"] never contains the substring "infigraph", so
        // a naive filter-then-append would duplicate it on every reinstall.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mcp.json");
        let fragment =
            r#"{"mcpServers":{"infigraph":{"command":"/bin/infigraph-mcp","args":["--mcp"]}}}"#;

        apply_json_deep_merge(&target, fragment).unwrap();
        apply_json_deep_merge(&target, fragment).unwrap();
        apply_json_deep_merge(&target, fragment).unwrap();

        let v = read_json(&target);
        let args = v["mcpServers"]["infigraph"]["args"].as_array().unwrap();
        assert_eq!(
            args.len(),
            1,
            "args should not duplicate across reinstalls, got {:?}",
            args
        );
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
        assert_eq!(
            arr.len(),
            1,
            "stale entry should be replaced, not duplicated"
        );
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
        assert_eq!(
            arr.len(),
            2,
            "other tool's entry must survive alongside ours"
        );
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
            ApplyOutcome::Skipped {
                reason,
                manual_snippet,
            } => {
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
        let fragment =
            r#"{"mcpServers":{"infigraph":{"command":"/bin/infigraph-mcp","args":["--mcp"]}}}"#;
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

    #[test]
    fn apply_marker_delimited_refuses_to_guess_when_end_marker_missing() {
        // Regression test: a damaged/hand-edited file with the start marker
        // but no matching end marker must not have its trailing content
        // silently destroyed by treating "no end marker" as "ends at EOF".
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let original = "# My notes\n<!-- infigraph-primary-search -->\nold block\n\n## Important content the user wrote after the block, with the end marker accidentally deleted\n";
        std::fs::write(&target, original).unwrap();

        let outcome = apply_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
            "new instructions",
        )
        .unwrap();

        match outcome {
            ApplyOutcome::Skipped { reason, .. } => {
                assert!(reason.contains("end marker"));
            }
            ApplyOutcome::Written => panic!("must not write when the end marker is missing"),
        }
        let after = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            after, original,
            "file must be completely untouched on refusal"
        );
    }

    #[test]
    fn remove_marker_delimited_refuses_to_guess_when_end_marker_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        let original = "<!-- infigraph-primary-search -->\nold block\n\n## User content after a damaged end marker\n";
        std::fs::write(&target, original).unwrap();

        let removed = remove_marker_delimited(
            &target,
            "<!-- infigraph-primary-search -->",
            "<!-- /infigraph-primary-search -->",
        )
        .unwrap();
        assert!(!removed);
        let after = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            after, original,
            "file must be completely untouched on refusal"
        );
    }
}
