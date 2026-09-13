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
