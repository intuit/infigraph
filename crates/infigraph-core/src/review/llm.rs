use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::graph::{GraphBackend, SymbolDetail};

use super::ReviewReport;

// ---------------------------------------------------------------------------
// Enriched report model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct EnrichedReport {
    pub base_report: ReviewReport,
    pub enriched_symbols: Vec<EnrichedSymbol>,
    pub file_diffs: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnrichedSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub change_kind: String,
    pub source: Option<String>,
    pub callers: Vec<String>,
    pub callees: Vec<String>,
    pub similar_symbols: Vec<SimilarSymbol>,
    pub complexity: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimilarSymbol {
    pub name: String,
    pub file: String,
    pub score: f32,
}

// ---------------------------------------------------------------------------
// LLM findings model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmReviewResult {
    pub summary: String,
    pub findings: Vec<LlmFinding>,
    pub test_plan: Vec<TestCase>,
    pub risk_assessment: Vec<RiskItem>,
    pub deployment_notes: Option<String>,
    pub token_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmFinding {
    pub file: String,
    pub line: Option<u32>,
    pub severity: String,
    pub category: String,
    pub message: String,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub category: String,
    pub priority: String,
    pub description: String,
    pub related_finding: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskItem {
    pub severity: String,
    pub area: String,
    pub description: String,
    pub affected_symbols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

// ---------------------------------------------------------------------------
// Enrichment: graph context per changed symbol
// ---------------------------------------------------------------------------

pub fn enrich_review(
    root: &Path,
    report: &ReviewReport,
    backend: &dyn GraphBackend,
) -> Result<EnrichedReport> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut enriched_symbols = Vec::new();

    for sym in &report.changed_symbols {
        let escaped_name = sym.name.replace('\'', "\\'");
        let escaped_file = sym.file.replace('\'', "\\'");

        // Resolve symbol ID
        let id_query = format!(
            "MATCH (s:Symbol) \
             WHERE s.name = '{escaped_name}' AND s.file ENDS WITH '{escaped_file}' \
             RETURN s.id, s.start_line, s.end_line, s.complexity"
        );
        let rows = backend.raw_query(&id_query).unwrap_or_default();
        let (symbol_id, complexity) = if let Some(row) = rows.first() {
            let id = row.first().cloned().unwrap_or_default();
            let cx: Option<u32> = row.get(3).and_then(|v| v.parse().ok());
            (id, cx)
        } else {
            (String::new(), None)
        };

        // Callers
        let callers = if !symbol_id.is_empty() {
            backend.callers_of(&symbol_id).unwrap_or_default()
        } else {
            vec![]
        };

        // Callees
        let callees = if !symbol_id.is_empty() {
            backend.callees_of(&symbol_id).unwrap_or_default()
        } else {
            vec![]
        };

        // Source snippet from graph
        let source = if !symbol_id.is_empty() {
            backend
                .find_symbol_by_id(&symbol_id)
                .ok()
                .flatten()
                .and_then(|detail| read_symbol_source(&canonical, &detail))
        } else {
            None
        };

        let similar_symbols = find_similar_symbols(backend, &sym.name, &sym.file);

        enriched_symbols.push(EnrichedSymbol {
            name: sym.name.clone(),
            kind: sym.kind.clone(),
            file: sym.file.clone(),
            change_kind: sym.change_kind.clone(),
            source,
            callers,
            callees,
            similar_symbols,
            complexity,
        });
    }

    // Git diff per changed file
    let file_diffs = collect_file_diffs(root, &report.base_ref)?;

    Ok(EnrichedReport {
        base_report: report.clone(),
        enriched_symbols,
        file_diffs,
    })
}

fn read_symbol_source(root: &Path, detail: &SymbolDetail) -> Option<String> {
    let file_path = root.join(&detail.file);
    let content = std::fs::read_to_string(&file_path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let start = detail.start_line.saturating_sub(1) as usize;
    let end = (detail.end_line as usize).min(lines.len());
    if start >= end {
        return None;
    }
    Some(lines[start..end].join("\n"))
}

fn find_similar_symbols(
    backend: &dyn GraphBackend,
    name: &str,
    exclude_file: &str,
) -> Vec<SimilarSymbol> {
    let escaped = name.replace('\'', "\\'");
    let query = format!(
        "MATCH (s:Symbol) \
         WHERE s.name CONTAINS '{escaped}' AND NOT s.file ENDS WITH '{exclude_file}' \
         RETURN s.name, s.file \
         LIMIT 5"
    );
    match backend.raw_query(&query) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                let n = row.first()?.clone();
                let f = row.get(1)?.clone();
                if n == name {
                    return None;
                }
                Some(SimilarSymbol {
                    name: n,
                    file: f,
                    score: 1.0,
                })
            })
            .collect(),
        Err(_) => vec![],
    }
}

fn collect_file_diffs(root: &Path, base_ref: &str) -> Result<HashMap<String, String>> {
    let output = std::process::Command::new("git")
        .args(["diff", "-U5", base_ref])
        .current_dir(root)
        .output()
        .context("git diff")?;

    let full_diff = String::from_utf8_lossy(&output.stdout);
    let mut diffs: HashMap<String, String> = HashMap::new();
    let mut current_file = String::new();
    let mut current_diff = String::new();

    for line in full_diff.lines() {
        if line.starts_with("diff --git") {
            if !current_file.is_empty() {
                diffs.insert(current_file.clone(), current_diff.clone());
            }
            current_file.clear();
            current_diff.clear();
        } else if line.starts_with("+++ b/") {
            current_file = line.strip_prefix("+++ b/").unwrap().to_string();
        }
        if !current_file.is_empty() {
            current_diff.push_str(line);
            current_diff.push('\n');
        }
    }
    if !current_file.is_empty() {
        diffs.insert(current_file, current_diff);
    }

    Ok(diffs)
}

// ---------------------------------------------------------------------------
// LLM review via Claude API
// ---------------------------------------------------------------------------

pub struct LlmConfig {
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub base_url: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 16384,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }
}

impl LlmConfig {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY not set")?;
        let model = std::env::var("INFIGRAPH_LLM_MODEL")
            .unwrap_or_else(|_| "claude-sonnet-4-20250514".to_string());
        let base_url = std::env::var("INFIGRAPH_LLM_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
        let max_tokens: u32 = std::env::var("INFIGRAPH_LLM_MAX_TOKENS")
            .unwrap_or_else(|_| "16384".to_string())
            .parse()
            .unwrap_or(16384);
        Ok(Self {
            api_key,
            model,
            max_tokens,
            base_url,
        })
    }
}

pub fn build_review_prompt(enriched: &EnrichedReport, context: Option<&str>) -> String {
    let mut prompt = String::with_capacity(32_000);

    prompt.push_str(
        "You are an expert code reviewer with access to the code knowledge graph. \
         You have callers, callees, similar code, complexity, and blast radius data \
         for each changed symbol. Use this to find issues that a diff-only reviewer would miss.\n\n");

    // Always inject auto-detected context
    prompt.push_str(&format!(
        "**Auto-detected context:** {}\n\
         **PR type:** {} | **Scope:** {} | **Files:** {} | **Symbols:** {}\n\n",
        enriched.base_report.context.inferred_intent,
        enriched.base_report.context.pr_type,
        enriched.base_report.context.scope,
        enriched.base_report.context.changed_file_count,
        enriched.base_report.context.changed_symbol_count,
    ));

    // User-provided context overrides/supplements auto-detected
    if let Some(ctx) = context {
        prompt.push_str(&format!(
            "**User-provided context:** {}\n\
             Prioritize the user's stated intent over auto-detection. \
             Flag anything that contradicts or undermines this goal.\n\n",
            ctx
        ));
    }

    // Scope-specific review instructions
    match enriched.base_report.context.scope {
        super::PrScope::CrossModule => {
            prompt.push_str(
                "This PR spans multiple modules. Pay special attention to:\n\
                 - Cross-module API contract violations\n\
                 - Shared state mutations that affect other modules\n\
                 - Import/dependency changes that could break build order\n\n",
            );
        }
        super::PrScope::CrossRepo => {
            prompt.push_str(
                "This PR is part of a cross-repo change. Pay special attention to:\n\
                 - Interface/COM/API compatibility across repos\n\
                 - Deployment ordering constraints\n\
                 - Cross-repo blast radius\n\
                 - Data format/schema compatibility\n\n",
            );
        }
        _ => {}
    }

    // Type-specific review instructions
    match enriched.base_report.context.pr_type {
        super::PrType::Migration => {
            prompt.push_str(
                "This is a MIGRATION PR. Critical review areas:\n\
                 - Data loss risk during migration\n\
                 - Rollback path — can the old system resume if migration fails?\n\
                 - Schema compatibility between old and new\n\
                 - NULL/default value handling differences\n\
                 - Performance under production data volume\n\n",
            );
        }
        super::PrType::BugFix => {
            prompt.push_str(
                "This is a BUG FIX PR. Focus on:\n\
                 - Does the fix actually address the root cause?\n\
                 - Could the fix introduce regressions in callers?\n\
                 - Is there a test that reproduces the bug?\n\n",
            );
        }
        super::PrType::Refactor => {
            prompt.push_str(
                "This is a REFACTOR PR. Focus on:\n\
                 - Behavioral equivalence — does the refactor preserve existing behavior?\n\
                 - Are all callers updated to use the new API?\n\
                 - Are there any callers in other repos not visible here?\n\n",
            );
        }
        _ => {}
    }

    prompt.push_str(
        "Respond ONLY with JSON in this exact format:\n\
         ```json\n\
         {\n\
           \"summary\": \"2-3 sentence PR summary\",\n\
           \"findings\": [\n\
             {\n\
               \"file\": \"path/to/file\",\n\
               \"line\": 42,\n\
               \"severity\": \"critical|high|medium|low|info\",\n\
               \"category\": \"bug|security|performance|logic|breaking_change|consistency|dead_code|duplication\",\n\
               \"message\": \"what is wrong and why\",\n\
               \"suggestion\": \"how to fix it\"\n\
             }\n\
           ],\n\
           \"test_plan\": [\n\
             {\n\
               \"category\": \"data_integrity|concurrency|regression|edge_case|integration|security\",\n\
               \"priority\": \"must_pass|should_pass|nice_to_have\",\n\
               \"description\": \"specific test scenario with inputs and expected output\",\n\
               \"related_finding\": 0\n\
             }\n\
           ],\n\
           \"risk_assessment\": [\n\
             {\n\
               \"severity\": \"high|medium|low\",\n\
               \"area\": \"short label (e.g. 'COM boundary', 'DI container', 'schema migration')\",\n\
               \"description\": \"what could go wrong and under what conditions\",\n\
               \"affected_symbols\": [\"ClassName.method\", \"OtherClass\"]\n\
             }\n\
           ],\n\
           \"deployment_notes\": \"ordering constraints, feature flags, rollback plan, migration steps. null if none.\"\n\
         }\n\
         ```\n\n\
         ## Review priorities\n\
         1. **Bugs and logic errors** — use callers/callees to check contract violations\n\
         2. **Breaking changes** — check every caller. Will they break?\n\
         3. **Consistency** — similar symbols that need the same change but weren't changed\n\
         4. **Concurrency** — thread safety of changed code, especially shared state\n\
         5. **Security** — injection, auth bypass, data exposure\n\
         6. **Data integrity** — type coercions, NULL handling, precision loss\n\
         7. **Dead code** — new functions/methods added with zero callers (unused code)\n\
         8. **Duplication** — near-identical functions that should be refactored into shared code\n\n\
         ## Test plan rules\n\
         Generate tests from THREE sources:\n\
         1. **Blast radius tests** — from callers/callees graph. If symbol X changed, every caller of X needs a test proving it still works.\n\
         2. **Logic permutation tests** — from the actual code. For each branch/condition, test all paths. For type coercions (e.g. StrToIntDef), test: valid input, empty string, null, negative, overflow, float-to-int. For boolean fields, test: true, false, null, 0, 1, -1.\n\
         3. **Consistency tests** — from similar symbols. If 19 entities share a pattern (e.g. COMtoBE), test that ALL 19 follow it. Flag any that diverge.\n\n\
         Additional rules:\n\
         - Generate specific, actionable test cases (not generic \"add tests\")\n\
         - Every critical/high finding MUST have at least one must_pass test case\n\
         - Include inputs and expected outputs where possible\n\
         - `related_finding` is the 0-based index into findings array. null if no related finding\n\
         - Cover: happy path, error path, boundary conditions, concurrency if applicable\n\
         - For data operations: test CRUD roundtrip, test with max-length strings, test with unicode\n\
         - For migrations: test upgrade path (old->new), test rollback, test partial failure recovery\n\n\
         ## What NOT to flag\n\
         Style nits, missing comments, naming preferences, formatting. Only actionable findings.\n\n\
         ---\n\n"
    );

    // Partition symbols: "interesting" (have graph connections) vs "bulk" (no connections)
    let (mut interesting, bulk): (Vec<&EnrichedSymbol>, Vec<&EnrichedSymbol>) =
        enriched.enriched_symbols.iter().partition(|s| {
            !s.callers.is_empty() || !s.callees.is_empty() || s.complexity.is_some_and(|c| c >= 10)
        });
    // Cap detailed symbols: prioritize by caller count + complexity
    let detail_cap = 100;
    if interesting.len() > detail_cap {
        interesting.sort_by(|a, b| {
            let score_a = a.callers.len() + a.callees.len() + a.complexity.unwrap_or(0) as usize;
            let score_b = b.callers.len() + b.callees.len() + b.complexity.unwrap_or(0) as usize;
            score_b.cmp(&score_a)
        });
        let overflow: Vec<&EnrichedSymbol> = interesting.split_off(detail_cap);
        // Re-merge overflow into bulk conceptually — add to prompt as summary
        if !overflow.is_empty() {
            prompt.push_str(&format!(
                "### Additional Symbols ({} with minor graph connections, summarized)\n\n",
                overflow.len()
            ));
            let mut by_file: HashMap<&str, Vec<&str>> = HashMap::new();
            for s in &overflow {
                by_file.entry(s.file.as_str()).or_default().push(&s.name);
            }
            let mut sorted: Vec<_> = by_file.into_iter().collect();
            sorted.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
            for (file, names) in sorted.iter().take(20) {
                prompt.push_str(&format!("- `{}`: {} symbols\n", file, names.len()));
            }
            prompt.push('\n');
        }
    }

    // Bulk symbols: group by file + change_kind, emit summary table
    if !bulk.is_empty() {
        let mut groups: HashMap<(String, String), Vec<String>> = HashMap::new();
        for s in &bulk {
            groups
                .entry((s.file.clone(), s.change_kind.clone()))
                .or_default()
                .push(format!("{} `{}`", s.kind, s.name));
        }

        prompt.push_str(&format!(
            "### Bulk Symbol Changes ({} symbols with no graph connections)\n\n",
            bulk.len()
        ));
        let mut sorted_groups: Vec<_> = groups.into_iter().collect();
        sorted_groups.sort_by(|a, b| a.0.cmp(&b.0));
        for ((file, change_kind), symbols) in &sorted_groups {
            prompt.push_str(&format!(
                "- `{}` ({}): {} symbols — {}\n",
                file,
                change_kind,
                symbols.len(),
                if symbols.len() <= 5 {
                    symbols.join(", ")
                } else {
                    format!(
                        "{}, ... and {} more",
                        symbols[..3].join(", "),
                        symbols.len() - 3
                    )
                }
            ));
        }
        prompt.push('\n');
    }

    // Interesting symbols: full detail
    if !interesting.is_empty() {
        prompt.push_str(&format!(
            "### Detailed Symbol Analysis ({} symbols with graph connections)\n\n",
            interesting.len()
        ));
    }
    for sym in &interesting {
        prompt.push_str(&format!(
            "#### {} `{}` in `{}` ({})\n\n",
            sym.kind, sym.name, sym.file, sym.change_kind
        ));

        if let Some(ref source) = sym.source {
            let truncated = if source.len() > 2000 {
                &source[..2000]
            } else {
                source.as_str()
            };
            prompt.push_str(&format!("**Current source:**\n```\n{}\n```\n\n", truncated));
        }

        if !sym.callers.is_empty() {
            let callers: Vec<&str> = sym.callers.iter().take(10).map(|s| s.as_str()).collect();
            prompt.push_str(&format!(
                "**Callers ({} total):** {}\n\n",
                sym.callers.len(),
                callers.join(", ")
            ));
        }

        if !sym.callees.is_empty() {
            let callees: Vec<&str> = sym.callees.iter().take(10).map(|s| s.as_str()).collect();
            prompt.push_str(&format!(
                "**Callees ({} total):** {}\n\n",
                sym.callees.len(),
                callees.join(", ")
            ));
        }

        if !sym.similar_symbols.is_empty() {
            prompt.push_str("**Similar code (may need same change):**\n");
            for s in &sym.similar_symbols {
                prompt.push_str(&format!(
                    "  - `{}` in `{}` (similarity: {:.2})\n",
                    s.name, s.file, s.score
                ));
            }
            prompt.push('\n');
        }

        if let Some(cx) = sym.complexity {
            prompt.push_str(&format!("**Complexity:** {}\n\n", cx));
        }
    }

    // File diffs — budget-capped, non-generated files first
    let diff_budget: usize = 80_000;
    let per_file_cap: usize = 2000;
    let mut sorted_diffs: Vec<(&String, &String)> = enriched.file_diffs.iter().collect();
    sorted_diffs.sort_by_key(|(f, _)| {
        if f.ends_with("_TLB.pas") || f.ends_with(".generated.cs") || f.ends_with(".g.cs") {
            1
        } else {
            0
        }
    });
    prompt.push_str(&format!(
        "---\n\n### File Diffs ({} files)\n\n",
        sorted_diffs.len()
    ));
    let mut diff_used: usize = 0;
    let mut skipped = 0usize;
    for (file, diff) in &sorted_diffs {
        if diff_used >= diff_budget {
            skipped += 1;
            continue;
        }
        let truncated = if diff.len() > per_file_cap {
            &diff[..per_file_cap]
        } else {
            diff.as_str()
        };
        diff_used += truncated.len();
        prompt.push_str(&format!("#### `{}`\n```diff\n{}\n```\n\n", file, truncated));
    }
    if skipped > 0 {
        prompt.push_str(&format!(
            "_{} files omitted (diff budget exceeded)_\n\n",
            skipped
        ));
    }

    // Security + complexity from base report
    if !enriched.base_report.security_findings.is_empty() {
        prompt.push_str("### Existing Security Findings (from static analysis)\n");
        for f in &enriched.base_report.security_findings {
            prompt.push_str(&format!(
                "  - [{}] {}:{} -- {}\n",
                f.severity, f.file, f.line, f.message
            ));
        }
        prompt.push('\n');
    }

    if !enriched.base_report.complexity_hotspots.is_empty() {
        prompt.push_str("### Complexity Hotspots\n");
        for h in &enriched.base_report.complexity_hotspots {
            prompt.push_str(&format!(
                "  - `{}` in `{}` (complexity: {})\n",
                h.name, h.file, h.complexity
            ));
        }
        prompt.push('\n');
    }

    if !enriched.base_report.dead_code.is_empty() {
        let dead = &enriched.base_report.dead_code;
        let cap = 50;
        prompt.push_str(&format!(
            "### Dead Code ({} symbols with zero callers)\n",
            dead.len()
        ));
        if dead.len() > cap {
            // Group by file for large sets
            let mut by_file: HashMap<&str, Vec<&str>> = HashMap::new();
            for d in dead {
                by_file.entry(d.file.as_str()).or_default().push(&d.name);
            }
            let mut sorted: Vec<_> = by_file.into_iter().collect();
            sorted.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
            for (file, names) in sorted.iter().take(20) {
                prompt.push_str(&format!("  - `{}`: {} symbols", file, names.len()));
                if names.len() <= 3 {
                    prompt.push_str(&format!(" -- {}\n", names.join(", ")));
                } else {
                    prompt.push_str(&format!(
                        " -- {}, ... +{}\n",
                        names[..3].join(", "),
                        names.len() - 3
                    ));
                }
            }
            if sorted.len() > 20 {
                prompt.push_str(&format!("  ... and {} more files\n", sorted.len() - 20));
            }
        } else {
            for d in dead {
                prompt.push_str(&format!("  - {} `{}` in `{}`\n", d.kind, d.name, d.file));
            }
        }
        prompt.push_str("\nNote: auto-generated files (*_TLB.pas, COM type libraries) often show as dead code because COM dispatch calls are invisible to the call graph. Focus on dead code in non-generated files.\n\n");
    }

    if !enriched.base_report.code_clones.is_empty() {
        prompt.push_str("### Code Clones (near-duplicate functions)\n");
        for c in &enriched.base_report.code_clones {
            prompt.push_str(&format!(
                "  - [{:.2}] `{}` ({}) <-> `{}` ({})\n",
                c.similarity, c.symbol_a, c.file_a, c.symbol_b, c.file_b,
            ));
        }
        prompt
            .push_str("\nSuggest refactoring clones into shared functions where appropriate.\n\n");
    }

    if !enriched.base_report.consistency_issues.is_empty() {
        prompt.push_str("### Consistency Issues (divergent patterns)\n");
        for ci in &enriched.base_report.consistency_issues {
            prompt.push_str(&format!(
                "  - Pattern: {} -- {}/{} consistent\n",
                ci.pattern, ci.actual_count, ci.expected_count,
            ));
            for o in &ci.outliers {
                prompt.push_str(&format!("    ! {}\n", o));
            }
        }
        prompt.push_str("\nFlag consistency violations — all instances of a pattern should follow the same structure.\n\n");
    }

    prompt
}

pub fn call_claude(config: &LlmConfig, prompt: &str) -> Result<LlmReviewResult> {
    let mut messages: Vec<serde_json::Value> =
        vec![serde_json::json!({"role": "user", "content": prompt})];
    let mut full_text = String::new();
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let max_continuations = 5;

    for attempt in 0..=max_continuations {
        let body = serde_json::json!({
            "model": config.model,
            "max_tokens": config.max_tokens,
            "messages": messages,
        });

        let resp = ureq::post(&format!("{}/v1/messages", config.base_url))
            .set("x-api-key", &config.api_key)
            .set("anthropic-version", "2023-06-01")
            .set("content-type", "application/json")
            .send_string(&body.to_string())
            .context("Claude API request failed")?;

        let resp_body: serde_json::Value = resp.into_json().context("parse Claude response")?;

        let chunk = resp_body["content"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|block| block["text"].as_str())
            .unwrap_or("");

        full_text.push_str(chunk);
        total_input += resp_body["usage"]["input_tokens"].as_u64().unwrap_or(0);
        total_output += resp_body["usage"]["output_tokens"].as_u64().unwrap_or(0);

        let stop_reason = resp_body["stop_reason"].as_str().unwrap_or("end_turn");

        if stop_reason != "max_tokens" || attempt == max_continuations {
            break;
        }

        // Truncated — ask LLM to continue
        messages.push(serde_json::json!({"role": "assistant", "content": chunk}));
        messages.push(serde_json::json!({"role": "user", "content": "Continue from where you left off. Complete the JSON."}));
    }

    let usage = TokenUsage {
        input_tokens: total_input,
        output_tokens: total_output,
    };

    let json_str = extract_json(&full_text);
    let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap_or_else(|_| {
        serde_json::json!({
            "summary": full_text,
            "findings": [],
            "test_plan": [],
            "risk_assessment": [],
            "deployment_notes": null
        })
    });

    let summary = parsed["summary"].as_str().unwrap_or("").to_string();
    let findings: Vec<LlmFinding> = parse_json_array(&parsed["findings"]);
    let test_plan: Vec<TestCase> = parse_json_array(&parsed["test_plan"]);
    let risk_assessment: Vec<RiskItem> = parse_json_array(&parsed["risk_assessment"]);
    let deployment_notes = parsed["deployment_notes"].as_str().map(|s| s.to_string());

    Ok(LlmReviewResult {
        summary,
        findings,
        test_plan,
        risk_assessment,
        deployment_notes,
        token_usage: Some(usage),
    })
}

fn parse_json_array<T: serde::de::DeserializeOwned>(val: &serde_json::Value) -> Vec<T> {
    val.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn extract_json(text: &str) -> &str {
    // Strip markdown code fences if present
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
}

/// Run full LLM-augmented review. Returns (prompt, Option<LlmReviewResult>).
/// In dry-run mode, returns the prompt and None.
pub fn review_with_llm(
    root: &Path,
    report: &ReviewReport,
    backend: &dyn GraphBackend,
    dry_run: bool,
    context: Option<&str>,
) -> Result<(String, Option<LlmReviewResult>)> {
    let enriched = enrich_review(root, report, backend)?;
    let prompt = build_review_prompt(&enriched, context);

    if dry_run {
        return Ok((prompt, None));
    }

    let config = LlmConfig::from_env()?;
    let result = call_claude(&config, &prompt)?;
    Ok((prompt, Some(result)))
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

pub fn format_llm_review(result: &LlmReviewResult) -> String {
    let mut out = String::new();

    out.push_str("## AI Review Summary\n\n");
    out.push_str(&result.summary);
    out.push_str("\n\n");

    // Findings
    if result.findings.is_empty() {
        out.push_str("### Findings\nNo issues found.\n\n");
    } else {
        out.push_str(&format!("### Findings ({})\n\n", result.findings.len()));
        for (i, f) in result.findings.iter().enumerate() {
            let location = match f.line {
                Some(line) => format!("{}:{}", f.file, line),
                None => f.file.clone(),
            };
            out.push_str(&format!(
                "  {}. [{}] **{}** {} -- {}\n",
                i + 1,
                f.severity.to_uppercase(),
                f.category,
                location,
                f.message,
            ));
            if let Some(ref suggestion) = f.suggestion {
                out.push_str(&format!("     -> {}\n", suggestion));
            }
        }
        out.push('\n');
    }

    // Risk Assessment
    if !result.risk_assessment.is_empty() {
        out.push_str(&format!(
            "### Risk Assessment ({})\n\n",
            result.risk_assessment.len()
        ));
        for r in &result.risk_assessment {
            out.push_str(&format!(
                "  [{}] **{}** -- {}\n",
                r.severity.to_uppercase(),
                r.area,
                r.description,
            ));
            if !r.affected_symbols.is_empty() {
                out.push_str(&format!(
                    "     Affects: {}\n",
                    r.affected_symbols.join(", "),
                ));
            }
        }
        out.push('\n');
    }

    // Test Plan
    if !result.test_plan.is_empty() {
        let must_pass = result
            .test_plan
            .iter()
            .filter(|t| t.priority == "must_pass")
            .count();
        let should_pass = result
            .test_plan
            .iter()
            .filter(|t| t.priority == "should_pass")
            .count();
        let nice = result.test_plan.len() - must_pass - should_pass;

        out.push_str(&format!(
            "### Test Plan ({} tests: {} must-pass, {} should-pass, {} nice-to-have)\n\n",
            result.test_plan.len(),
            must_pass,
            should_pass,
            nice,
        ));

        for bucket in &["must_pass", "should_pass", "nice_to_have"] {
            let tests: Vec<&TestCase> = result
                .test_plan
                .iter()
                .filter(|t| t.priority == *bucket)
                .collect();
            if tests.is_empty() {
                continue;
            }
            let label = bucket.replace('_', "-");
            out.push_str(&format!("  **{}:**\n", label));
            for t in tests {
                let finding_ref = match t.related_finding {
                    Some(idx) => format!(" (-> finding #{})", idx + 1),
                    None => String::new(),
                };
                out.push_str(&format!(
                    "    - [{}] {}{}\n",
                    t.category, t.description, finding_ref,
                ));
            }
        }
        out.push('\n');
    }

    // Deployment Notes
    if let Some(ref notes) = result.deployment_notes {
        if !notes.is_empty() {
            out.push_str("### Deployment Notes\n\n");
            out.push_str(notes);
            out.push_str("\n\n");
        }
    }

    if let Some(ref usage) = result.token_usage {
        out.push_str(&format!(
            "_Tokens: {} in / {} out_\n",
            usage.input_tokens, usage.output_tokens,
        ));
    }

    out
}

pub fn format_llm_review_json(result: &LlmReviewResult) -> String {
    serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string())
}
