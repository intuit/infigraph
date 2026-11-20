use std::path::Path;

use anyhow::Result;

const VERSION: u32 = 3;

/// Write/update project-level `.claude/CLAUDE.md` with infigraph instructions.
/// Uses sentinel markers for idempotent managed-block replacement.
pub fn ensure_project_claude_md(project_root: &Path) -> Result<()> {
    let claude_dir = project_root.join(".claude");
    let claude_md = claude_dir.join("CLAUDE.md");
    let begin_marker = format!("<!-- BEGIN INFIGRAPH v{} -->", VERSION);
    let end_marker = "<!-- END INFIGRAPH -->";

    let instructions = format!(
        r#"
{begin_marker}
## Infigraph — Code Intelligence (auto-generated)

This project is indexed by Infigraph, which is available here as the `infigraph` MCP server.
Its tools query a prebuilt code graph, so for code questions they are usually faster and more
precise than grep. Plain grep/Read remain fine for non-code files or when Infigraph returns nothing.

### Suggested tool choices
- `search` for code search: combines keyword, semantic and regex matching in one call.
- `get_doc_context` before editing a function: returns its source, callers and callees together.
- `trace_callers` / `find_all_references` before refactoring: grep can miss indirect callers.
- `trace_callees` / `transitive_impact` to estimate blast radius.
- Read files directly for non-code files, or when the Edit tool needs line numbers.

### Subagents
Explore, Plan and code-reviewer subagents don't have access to MCP tools, so in this project they
fall back to grep. A general-purpose subagent does have access. For simple lookups, calling
`search`, `get_architecture` or `get_doc_context` directly avoids a subagent altogether.

### Large outputs
These tools can return a lot of text, so running them in a subagent keeps the main context small:
`get_architecture`, `transitive_impact`, `detect_dead_code`, `detect_clusters`, `detect_clones`,
`export_graph`, `query_graph`, deep `trace_callers`/`trace_callees`, `group_query`, `group_index`.

### Context compression
Infigraph compresses its own tool output to save context, and nothing needs to be done for it.
- The level grows with session length (Off → Summary → Aggressive → Minimal).
- `search` results stay at Summary level at most, to keep result quality.
- Security tools (`detect_security_issues`, `detect_taint_flows`, etc.) are never compressed.
- `get_code_snippet` is never compressed, so edits see the exact source.
{end_marker}
"#
    );

    let existing = std::fs::read_to_string(&claude_md).unwrap_or_default();

    if existing.contains(&begin_marker) {
        return Ok(());
    }

    let new_content = if let Some(start) = existing.find("<!-- BEGIN INFIGRAPH") {
        if let Some(end_pos) = existing[start..].find(end_marker) {
            let end = start + end_pos + end_marker.len();
            let end = if existing[end..].starts_with('\n') {
                end + 1
            } else {
                end
            };
            format!("{}{}{}", &existing[..start], instructions, &existing[end..])
        } else {
            format!("{}\n{}", existing, instructions)
        }
    } else if existing.is_empty() {
        std::fs::create_dir_all(&claude_dir)?;
        instructions.to_string()
    } else {
        format!("{}\n{}", existing, instructions)
    };

    std::fs::create_dir_all(&claude_dir)?;
    std::fs::write(&claude_md, new_content)?;
    println!("  Updated project CLAUDE.md ({})", claude_md.display());
    Ok(())
}

/// Remove the managed infigraph block from project `.claude/CLAUDE.md`.
/// Returns true if a block was removed, false if nothing to do.
pub fn remove_project_claude_md(project_root: &Path) -> Result<bool> {
    let claude_md = project_root.join(".claude").join("CLAUDE.md");
    let end_marker = "<!-- END INFIGRAPH -->";

    let existing = match std::fs::read_to_string(&claude_md) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };

    let start = match existing.find("<!-- BEGIN INFIGRAPH") {
        Some(s) => s,
        None => return Ok(false),
    };

    let end = match existing[start..].find(end_marker) {
        Some(p) => {
            let e = start + p + end_marker.len();
            if existing[e..].starts_with('\n') {
                e + 1
            } else {
                e
            }
        }
        None => return Ok(false),
    };

    let new_content = format!("{}{}", &existing[..start], &existing[end..]);
    let trimmed = new_content.trim();
    if trimmed.is_empty() {
        std::fs::remove_file(&claude_md)?;
    } else {
        std::fs::write(&claude_md, format!("{}\n", trimmed))?;
    }
    Ok(true)
}
