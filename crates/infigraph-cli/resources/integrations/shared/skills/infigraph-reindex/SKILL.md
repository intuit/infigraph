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
