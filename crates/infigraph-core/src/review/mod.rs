//! PR review mode — combines changed symbols, blast radius, affected tests,
//! API surface changes, security scan, and complexity hotspots into one
//! structured report.
//!
//! Usage:
//! ```text
//! infigraph review              # diff HEAD~1..HEAD
//! infigraph review --base main  # diff main..HEAD
//! infigraph review --json       # JSON output
//! ```

pub mod llm;

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::diff;
use crate::graph::GraphBackend;
use crate::lang::LanguageRegistry;
use crate::security;

// ---------------------------------------------------------------------------
// Report model
// ---------------------------------------------------------------------------

/// The complete PR review report.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewReport {
    pub base_ref: String,
    pub context: ReviewContext,
    pub changed_symbols: Vec<ChangedSymbol>,
    pub blast_radius: Vec<AffectedSymbol>,
    pub affected_tests: Vec<AffectedSymbol>,
    pub api_surface_changes: Vec<ChangedSymbol>,
    pub security_findings: Vec<SecurityFinding>,
    pub complexity_hotspots: Vec<ComplexityHotspot>,
    pub dead_code: Vec<DeadCodeSymbol>,
    pub code_clones: Vec<ClonePair>,
    pub consistency_issues: Vec<ConsistencyIssue>,
}

/// A symbol that changed between the base and HEAD.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub change_kind: String,
}

/// A symbol affected by the changes (in the blast radius or test list).
#[derive(Debug, Clone, Serialize)]
pub struct AffectedSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
}

/// A security finding scoped to the changed files.
#[derive(Debug, Clone, Serialize)]
pub struct SecurityFinding {
    pub file: String,
    pub line: u32,
    pub severity: String,
    pub message: String,
}

/// A high-complexity symbol in the changed files.
#[derive(Debug, Clone, Serialize)]
pub struct ComplexityHotspot {
    pub name: String,
    pub file: String,
    pub complexity: u32,
}

/// A symbol in changed files with zero callers.
#[derive(Debug, Clone, Serialize)]
pub struct DeadCodeSymbol {
    pub name: String,
    pub kind: String,
    pub file: String,
}

/// A pair of near-duplicate functions in changed files.
#[derive(Debug, Clone, Serialize)]
pub struct ClonePair {
    pub symbol_a: String,
    pub file_a: String,
    pub symbol_b: String,
    pub file_b: String,
    pub similarity: f32,
}

/// A pattern consistency issue — symbols that should follow a common pattern but diverge.
#[derive(Debug, Clone, Serialize)]
pub struct ConsistencyIssue {
    pub pattern: String,
    pub expected_count: usize,
    pub actual_count: usize,
    pub outliers: Vec<String>,
}

/// Auto-detected PR context that drives review depth.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewContext {
    pub pr_type: PrType,
    pub scope: PrScope,
    pub inferred_intent: String,
    pub changed_file_count: usize,
    pub changed_symbol_count: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum PrType {
    BugFix,
    Refactor,
    Feature,
    Migration,
    Config,
    Test,
    Docs,
    Mixed,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum PrScope {
    Standalone,
    CrossModule,
    CrossRepo,
}

impl std::fmt::Display for PrType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrType::BugFix => write!(f, "bug-fix"),
            PrType::Refactor => write!(f, "refactor"),
            PrType::Feature => write!(f, "feature"),
            PrType::Migration => write!(f, "migration"),
            PrType::Config => write!(f, "config"),
            PrType::Test => write!(f, "test"),
            PrType::Docs => write!(f, "docs"),
            PrType::Mixed => write!(f, "mixed"),
        }
    }
}

impl std::fmt::Display for PrScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrScope::Standalone => write!(f, "standalone"),
            PrScope::CrossModule => write!(f, "cross-module"),
            PrScope::CrossRepo => write!(f, "cross-repo"),
        }
    }
}

// ---------------------------------------------------------------------------
// Core review logic
// ---------------------------------------------------------------------------

/// Run the full PR review pipeline and produce a structured report.
pub fn review(
    root: &Path,
    base_ref: &str,
    limit: usize,
    registry: &LanguageRegistry,
    backend: &dyn GraphBackend,
) -> Result<ReviewReport> {
    let canonical = root.canonicalize().context("invalid project root")?;

    // 1. Get changed files via git
    let changed_files = git_changed_files(&canonical, base_ref)?;
    if changed_files.is_empty() {
        return Ok(ReviewReport {
            base_ref: base_ref.to_string(),
            context: ReviewContext {
                pr_type: PrType::Mixed,
                scope: PrScope::Standalone,
                inferred_intent: "No changes detected".to_string(),
                changed_file_count: 0,
                changed_symbol_count: 0,
            },
            changed_symbols: vec![],
            blast_radius: vec![],
            affected_tests: vec![],
            api_surface_changes: vec![],
            security_findings: vec![],
            complexity_hotspots: vec![],
            dead_code: vec![],
            code_clones: vec![],
            consistency_issues: vec![],
        });
    }

    // 2. Semantic diff: get changed symbols
    let symbol_diff =
        diff::semantic_diff(&canonical, base_ref, "HEAD", registry).unwrap_or_default();

    let changed_symbols: Vec<ChangedSymbol> = symbol_diff
        .changes
        .iter()
        .map(|c| ChangedSymbol {
            name: c.name.clone(),
            kind: c.kind.clone(),
            file: c.file.clone(),
            change_kind: c.change.to_string(),
        })
        .collect();

    // 2b. Auto-detect PR context from changes
    let context = detect_pr_context(&canonical, base_ref, &changed_files, &changed_symbols);

    // 3a. Resolve changed symbol IDs in the graph
    let symbol_ids = resolve_symbol_ids(backend, &changed_symbols);

    // 3b. Blast radius: unbounded CALLS* traversal for each changed symbol
    let mut blast_set: HashSet<String> = HashSet::new();
    let mut blast_radius: Vec<AffectedSymbol> = Vec::new();

    for id in &symbol_ids {
        let escaped = id.replace('\'', "\\'");
        let query = format!(
            "MATCH (s:Symbol)<-[:CALLS*]-(a:Symbol) \
             WHERE s.id = '{escaped}' \
             RETURN DISTINCT a.name, a.kind, a.file \
             LIMIT {limit}",
        );
        if let Ok(rows) = backend.raw_query(&query) {
            for row in rows {
                if row.len() >= 3 {
                    let key = format!("{}::{}", row[2], row[0]);
                    if blast_set.insert(key) {
                        blast_radius.push(AffectedSymbol {
                            name: row[0].clone(),
                            kind: row[1].clone(),
                            file: row[2].clone(),
                        });
                    }
                }
            }
        }
    }

    // Cap total blast radius
    blast_radius.truncate(limit);

    // 3c. Affected tests: filter blast radius for test symbols
    let affected_tests: Vec<AffectedSymbol> = blast_radius
        .iter()
        .filter(|s| is_test_symbol(s))
        .cloned()
        .collect();

    // 3d. API surface changes: filter changed symbols that are public
    let api_surface_changes = find_api_surface_changes(backend, &changed_symbols);

    // 4. Security scan scoped to changed files
    let security_findings = scan_changed_files(&canonical, &changed_files);

    // 5. Complexity hotspots in changed files
    let complexity_hotspots = find_complexity_hotspots(backend, &changed_files);

    // 6. Dead code in changed files (symbols with zero callers)
    let dead_code = find_dead_code_in_changed_files(backend, &changed_files);

    // 7. Code clones: near-duplicate symbols in changed files (via SIMILAR_TO edges)
    let code_clones = find_clones_in_changed_files(backend, &changed_files);

    // 8. Consistency: symbols sharing a name pattern that diverge in structure
    let consistency_issues = find_consistency_issues(backend, &changed_symbols);

    Ok(ReviewReport {
        base_ref: base_ref.to_string(),
        context,
        changed_symbols,
        blast_radius,
        affected_tests,
        api_surface_changes,
        security_findings,
        complexity_hotspots,
        dead_code,
        code_clones,
        consistency_issues,
    })
}

/// Cross-repo review: runs single-repo review, then enriches with cross-repo
/// blast radius and callers from the group's other repos.
#[allow(clippy::too_many_arguments)]
pub fn review_with_group(
    root: &Path,
    base_ref: &str,
    limit: usize,
    registry: &LanguageRegistry,
    backend: &dyn GraphBackend,
    group_name: &str,
    group_registry: &crate::multi::Registry,
    build_registry: impl Fn() -> Result<LanguageRegistry>,
) -> Result<ReviewReport> {
    // 1. Run standard single-repo review
    let mut report = review(root, base_ref, limit, registry, backend)?;

    // 2. Force scope to CrossRepo
    report.context.scope = PrScope::CrossRepo;
    report.context.inferred_intent = format!(
        "cross-repo {} PR (group: {}): {}",
        report.context.pr_type, group_name, report.context.inferred_intent,
    );

    // 3. Query cross-repo callers for each changed symbol
    let mut cross_repo_blast: Vec<AffectedSymbol> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for sym in &report.changed_symbols {
        let escaped_name = sym.name.replace('\'', "\\'");
        let query = format!(
            "MATCH (s:Symbol)-[:CALLS]->(t:Symbol) \
             WHERE t.name = '{escaped_name}' \
             RETURN s.name, s.kind, s.file \
             LIMIT 50"
        );

        if let Ok(results) = group_registry.group_query(group_name, &query, &build_registry) {
            for (repo_name, rows) in results {
                for row in rows {
                    if row.len() >= 3 {
                        let key = format!("{}::{}::{}", repo_name, row[2], row[0]);
                        if seen.insert(key) {
                            cross_repo_blast.push(AffectedSymbol {
                                name: row[0].clone(),
                                kind: row[1].clone(),
                                file: format!("[{}] {}", repo_name, row[2]),
                            });
                        }
                    }
                }
            }
        }
    }

    // 4. Append cross-repo blast radius
    report.blast_radius.extend(cross_repo_blast);

    // 5. Re-filter affected tests from expanded blast radius
    let cross_repo_tests: Vec<AffectedSymbol> = report
        .blast_radius
        .iter()
        .filter(|s| is_test_symbol(s) && s.file.starts_with('['))
        .cloned()
        .collect();
    report.affected_tests.extend(cross_repo_tests);

    // 6. Filter dead code against cross-repo callers and implementors
    if !report.dead_code.is_empty() {
        let mut alive_names: HashSet<String> = HashSet::new();
        for dc in &report.dead_code {
            let escaped_name = dc.name.replace('\'', "\\'");
            // Check for cross-repo CALLS
            let calls_query = format!(
                "MATCH (s:Symbol)-[:CALLS]->(t:Symbol) \
                 WHERE t.name = '{escaped_name}' \
                 RETURN t.name LIMIT 1"
            );
            if let Ok(results) =
                group_registry.group_query(group_name, &calls_query, &build_registry)
            {
                if results.iter().any(|(_, rows)| !rows.is_empty()) {
                    alive_names.insert(dc.name.clone());
                    continue;
                }
            }
            // Check for cross-repo INHERITS (interface implemented in another repo)
            let inh_query = format!(
                "MATCH (s:Symbol)-[:INHERITS]->(p:Symbol) \
                 WHERE p.name = '{escaped_name}' OR \
                 EXISTS {{ MATCH (m:Symbol) WHERE m.name = '{escaped_name}' AND m.parent = p.id }} \
                 RETURN s.name LIMIT 1"
            );
            if let Ok(results) = group_registry.group_query(group_name, &inh_query, &build_registry)
            {
                if results.iter().any(|(_, rows)| !rows.is_empty()) {
                    alive_names.insert(dc.name.clone());
                }
            }
        }
        if !alive_names.is_empty() {
            report
                .dead_code
                .retain(|dc| !alive_names.contains(&dc.name));
        }
    }

    // 7. Cross-repo consistency: same symbol name across repos should match
    let mut cross_repo_names: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for sym in &report.changed_symbols {
        let query = format!(
            "MATCH (s:Symbol) WHERE s.name = '{}' RETURN s.name, s.file",
            sym.name.replace('\'', "\\'")
        );
        if let Ok(results) = group_registry.group_query(group_name, &query, &build_registry) {
            for (repo_name, rows) in results {
                for row in &rows {
                    if let Some(file) = row.get(1) {
                        cross_repo_names
                            .entry(sym.name.clone())
                            .or_default()
                            .push(format!("[{}] {}", repo_name, file));
                    }
                }
            }
        }
    }

    for (name, locations) in &cross_repo_names {
        if locations.len() >= 2 {
            report.consistency_issues.push(ConsistencyIssue {
                pattern: format!(
                    "{} exists in {} repos — verify all updated",
                    name,
                    locations.len()
                ),
                expected_count: locations.len(),
                actual_count: 0,
                outliers: locations.clone(),
            });
        }
    }

    Ok(report)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get the list of files changed between `base_ref` and HEAD.
fn git_changed_files(root: &Path, base_ref: &str) -> Result<Vec<String>> {
    let check = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(root)
        .output();
    if check.is_err() || !check.unwrap().status.success() {
        anyhow::bail!("not a git repository — infigraph review requires git history");
    }

    let output = Command::new("git")
        .args(["diff", "--name-only", base_ref])
        .current_dir(root)
        .output()
        .context("failed to run git diff --name-only")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {stderr}");
    }

    let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(files)
}

/// Resolve graph symbol IDs for changed symbols by querying the graph.
fn resolve_symbol_ids(backend: &dyn GraphBackend, symbols: &[ChangedSymbol]) -> Vec<String> {
    let mut ids = Vec::new();
    for sym in symbols {
        let escaped_name = sym.name.replace('\'', "\\'");
        let escaped_file = sym.file.replace('\'', "\\'");
        let query = format!(
            "MATCH (s:Symbol) \
             WHERE s.name = '{escaped_name}' AND s.file ENDS WITH '{escaped_file}' \
             RETURN s.id",
        );
        if let Ok(rows) = backend.raw_query(&query) {
            for row in rows {
                if let Some(id) = row.first() {
                    ids.push(id.clone());
                }
            }
        }
    }
    ids
}

/// Check if a symbol looks like a test.
fn is_test_symbol(sym: &AffectedSymbol) -> bool {
    let name_lower = sym.name.to_lowercase();
    let kind_lower = sym.kind.to_lowercase();
    name_lower.starts_with("test_")
        || name_lower.starts_with("test")
        || kind_lower.contains("test")
        || sym.file.contains("test")
        || sym.file.contains("spec")
}

/// Find changed symbols that are public (API surface).
fn find_api_surface_changes(
    backend: &dyn GraphBackend,
    symbols: &[ChangedSymbol],
) -> Vec<ChangedSymbol> {
    let mut api_changes = Vec::new();
    for sym in symbols {
        let escaped_name = sym.name.replace('\'', "\\'");
        let escaped_file = sym.file.replace('\'', "\\'");
        let query = format!(
            "MATCH (s:Symbol) \
             WHERE s.name = '{escaped_name}' AND s.file ENDS WITH '{escaped_file}' \
             AND s.visibility = 'public' \
             RETURN s.name",
        );
        if let Ok(rows) = backend.raw_query(&query) {
            if !rows.is_empty() {
                api_changes.push(sym.clone());
            }
        }
    }
    api_changes
}

/// Run security scan and filter to only findings in changed files.
fn scan_changed_files(root: &Path, changed_files: &[String]) -> Vec<SecurityFinding> {
    let changed_set: HashSet<&str> = changed_files.iter().map(|f| f.as_str()).collect();

    match security::scan_project(root) {
        Ok(scan) => scan
            .findings
            .iter()
            .filter(|f| changed_set.contains(f.file.as_str()))
            .map(|f| SecurityFinding {
                file: f.file.clone(),
                line: f.line,
                severity: f.severity.to_string(),
                message: f.message.clone(),
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// Find high-complexity symbols in changed files.
fn find_complexity_hotspots(
    backend: &dyn GraphBackend,
    changed_files: &[String],
) -> Vec<ComplexityHotspot> {
    if changed_files.is_empty() {
        return vec![];
    }

    let file_list: Vec<String> = changed_files
        .iter()
        .map(|f| format!("'{}'", f.replace('\'', "\\'")))
        .collect();
    let files_in = file_list.join(", ");

    let query = format!(
        "MATCH (s:Symbol) \
         WHERE s.file IN [{files_in}] AND s.complexity >= 10 \
         RETURN s.name, s.file, s.complexity \
         ORDER BY s.complexity DESC",
    );

    match backend.raw_query(&query) {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                let name = row.first()?;
                let file = row.get(1)?;
                let complexity: u32 = row.get(2)?.parse().ok()?;
                Some(ComplexityHotspot {
                    name: name.clone(),
                    file: file.clone(),
                    complexity,
                })
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// Auto-detect PR type, scope, and intent from changes.
fn detect_pr_context(
    root: &Path,
    base_ref: &str,
    changed_files: &[String],
    changed_symbols: &[ChangedSymbol],
) -> ReviewContext {
    let file_count = changed_files.len();
    let symbol_count = changed_symbols.len();

    // Detect PR type from commit messages and file patterns
    let pr_type = detect_pr_type(root, base_ref, changed_files, changed_symbols);

    // Detect scope: how many directories/modules are touched?
    let scope = detect_pr_scope(changed_files);

    // Build inferred intent string
    let intent = build_intent_string(&pr_type, &scope, changed_files, changed_symbols);

    ReviewContext {
        pr_type,
        scope,
        inferred_intent: intent,
        changed_file_count: file_count,
        changed_symbol_count: symbol_count,
    }
}

fn detect_pr_type(
    root: &Path,
    base_ref: &str,
    changed_files: &[String],
    changed_symbols: &[ChangedSymbol],
) -> PrType {
    // Get commit messages for signal
    let commit_msgs = Command::new("git")
        .args(["log", "--format=%s", &format!("{}..HEAD", base_ref)])
        .current_dir(root)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default();

    // Score each type
    let mut scores: Vec<(PrType, i32)> = vec![
        (PrType::BugFix, 0),
        (PrType::Refactor, 0),
        (PrType::Feature, 0),
        (PrType::Migration, 0),
        (PrType::Config, 0),
        (PrType::Test, 0),
        (PrType::Docs, 0),
    ];

    // Commit message signals
    for (pr_type, score) in &mut scores {
        match pr_type {
            PrType::BugFix
                if commit_msgs.contains("fix")
                    || commit_msgs.contains("bug")
                    || commit_msgs.contains("patch") =>
            {
                *score += 3;
            }
            PrType::Refactor
                if commit_msgs.contains("refactor")
                    || commit_msgs.contains("rename")
                    || commit_msgs.contains("move")
                    || commit_msgs.contains("clean") =>
            {
                *score += 3;
            }
            PrType::Feature
                if commit_msgs.contains("add")
                    || commit_msgs.contains("new")
                    || commit_msgs.contains("feature")
                    || commit_msgs.contains("implement") =>
            {
                *score += 3;
            }
            PrType::Migration
                if commit_msgs.contains("migrat")
                    || commit_msgs.contains("upgrade")
                    || commit_msgs.contains("convert")
                    || commit_msgs.contains("sqlite") =>
            {
                *score += 5;
            }
            PrType::Config
                if commit_msgs.contains("config")
                    || commit_msgs.contains("setting")
                    || commit_msgs.contains("version bump") =>
            {
                *score += 3;
            }
            PrType::Test if commit_msgs.contains("test") => {
                *score += 3;
            }
            PrType::Docs if commit_msgs.contains("doc") || commit_msgs.contains("readme") => {
                *score += 3;
            }
            _ => {}
        }
    }

    // File pattern signals
    let test_files = changed_files
        .iter()
        .filter(|f| f.contains("test") || f.contains("spec"))
        .count();
    let config_files = changed_files
        .iter()
        .filter(|f| {
            f.ends_with(".json")
                || f.ends_with(".xml")
                || f.ends_with(".yaml")
                || f.ends_with(".yml")
                || f.ends_with(".csproj")
                || f.ends_with(".sln")
                || f.ends_with(".cfg")
                || f.ends_with(".pkg")
        })
        .count();
    let doc_files = changed_files
        .iter()
        .filter(|f| f.ends_with(".md") || f.ends_with(".txt") || f.ends_with(".rst"))
        .count();
    let schema_files = changed_files
        .iter()
        .filter(|f| f.contains("schema") || f.contains("migration") || f.contains("sql"))
        .count();

    if test_files as f32 / changed_files.len().max(1) as f32 > 0.7 {
        scores
            .iter_mut()
            .find(|(t, _)| *t == PrType::Test)
            .unwrap()
            .1 += 5;
    }
    if config_files as f32 / changed_files.len().max(1) as f32 > 0.7 {
        scores
            .iter_mut()
            .find(|(t, _)| *t == PrType::Config)
            .unwrap()
            .1 += 5;
    }
    if doc_files as f32 / changed_files.len().max(1) as f32 > 0.7 {
        scores
            .iter_mut()
            .find(|(t, _)| *t == PrType::Docs)
            .unwrap()
            .1 += 5;
    }
    if schema_files > 0 {
        scores
            .iter_mut()
            .find(|(t, _)| *t == PrType::Migration)
            .unwrap()
            .1 += 3;
    }

    // Symbol change signals
    let moved = changed_symbols
        .iter()
        .filter(|s| s.change_kind.starts_with("MOVED"))
        .count();
    let removed = changed_symbols
        .iter()
        .filter(|s| s.change_kind == "REMOVED")
        .count();
    let added_count = changed_symbols
        .iter()
        .filter(|s| s.change_kind == "ADDED")
        .count();

    if moved as f32 / symbol_count_safe(changed_symbols) > 0.3 {
        scores
            .iter_mut()
            .find(|(t, _)| *t == PrType::Refactor)
            .unwrap()
            .1 += 3;
    }
    if added_count as f32 / symbol_count_safe(changed_symbols) > 0.5 {
        scores
            .iter_mut()
            .find(|(t, _)| *t == PrType::Feature)
            .unwrap()
            .1 += 3;
    }
    if removed as f32 / symbol_count_safe(changed_symbols) > 0.3 {
        scores
            .iter_mut()
            .find(|(t, _)| *t == PrType::Refactor)
            .unwrap()
            .1 += 2;
    }

    scores.sort_by_key(|a| std::cmp::Reverse(a.1));
    if scores[0].1 == 0 {
        PrType::Mixed
    } else {
        scores[0].0.clone()
    }
}

fn symbol_count_safe(symbols: &[ChangedSymbol]) -> f32 {
    (symbols.len().max(1)) as f32
}

fn detect_pr_scope(changed_files: &[String]) -> PrScope {
    let dirs: HashSet<&str> = changed_files
        .iter()
        .filter_map(|f| f.split('/').next())
        .collect();

    if dirs.len() <= 2 {
        PrScope::Standalone
    } else {
        PrScope::CrossModule
    }
}

fn build_intent_string(
    pr_type: &PrType,
    scope: &PrScope,
    changed_files: &[String],
    changed_symbols: &[ChangedSymbol],
) -> String {
    let added = changed_symbols
        .iter()
        .filter(|s| s.change_kind == "ADDED")
        .count();
    let removed = changed_symbols
        .iter()
        .filter(|s| s.change_kind == "REMOVED")
        .count();
    let modified = changed_symbols
        .iter()
        .filter(|s| s.change_kind == "SIGNATURE_CHANGED")
        .count();
    let moved = changed_symbols
        .iter()
        .filter(|s| s.change_kind.starts_with("MOVED"))
        .count();

    let file_types: HashSet<&str> = changed_files
        .iter()
        .filter_map(|f| f.rsplit('.').next())
        .collect();
    let langs: Vec<&&str> = file_types.iter().take(5).collect();

    format!(
        "{} {} PR: {} files ({}) changed, {} symbols (+{} -{} ~{} →{})",
        scope,
        pr_type,
        changed_files.len(),
        langs
            .iter()
            .map(|l| format!(".{}", l))
            .collect::<Vec<_>>()
            .join(", "),
        changed_symbols.len(),
        added,
        removed,
        modified,
        moved,
    )
}

/// Find functions/methods in changed files that have zero callers.
fn find_dead_code_in_changed_files(
    backend: &dyn GraphBackend,
    changed_files: &[String],
) -> Vec<DeadCodeSymbol> {
    if changed_files.is_empty() {
        return vec![];
    }

    let file_list: Vec<String> = changed_files
        .iter()
        .map(|f| format!("'{}'", f.replace('\'', "\\'")))
        .collect();
    let files_in = file_list.join(", ");

    let query = format!(
        "MATCH (s:Symbol) \
         WHERE s.file IN [{files_in}] \
         AND s.kind IN ['Function', 'Method'] \
         AND NOT EXISTS {{ MATCH ()-[:CALLS]->(s) }} \
         AND NOT EXISTS {{ MATCH (p:Symbol)<-[:INHERITS]-() WHERE p.file = s.file AND p.kind IN ['Class', 'Interface', 'Trait'] }} \
         AND NOT s.name STARTS WITH 'test' \
         AND NOT s.name STARTS WITH 'Test' \
         AND NOT s.name = 'main' \
         RETURN s.name, s.kind, s.file \
         ORDER BY s.file, s.name"
    );

    match backend.raw_query(&query) {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                Some(DeadCodeSymbol {
                    name: row.first()?.clone(),
                    kind: row.get(1)?.clone(),
                    file: row.get(2)?.clone(),
                })
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// Find near-duplicate symbols in changed files using SIMILAR_TO edges.
fn find_clones_in_changed_files(
    backend: &dyn GraphBackend,
    changed_files: &[String],
) -> Vec<ClonePair> {
    if changed_files.is_empty() {
        return vec![];
    }

    let file_list: Vec<String> = changed_files
        .iter()
        .map(|f| format!("'{}'", f.replace('\'', "\\'")))
        .collect();
    let files_in = file_list.join(", ");

    let query = format!(
        "MATCH (a:Symbol)-[r:SIMILAR_TO]->(b:Symbol) \
         WHERE a.file IN [{files_in}] \
         AND r.score >= 0.90 \
         RETURN a.name, a.file, b.name, b.file, r.score \
         ORDER BY r.score DESC \
         LIMIT 30"
    );

    match backend.raw_query(&query) {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                Some(ClonePair {
                    symbol_a: row.first()?.clone(),
                    file_a: row.get(1)?.clone(),
                    symbol_b: row.get(2)?.clone(),
                    file_b: row.get(3)?.clone(),
                    similarity: row.get(4)?.parse().ok()?,
                })
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// Find consistency issues: groups of changed symbols with similar names
/// that should follow the same pattern but have different structures.
fn find_consistency_issues(
    backend: &dyn GraphBackend,
    changed_symbols: &[ChangedSymbol],
) -> Vec<ConsistencyIssue> {
    let mut issues = Vec::new();

    // Group symbols by name — find cases where the same method exists in multiple files
    let mut name_groups: std::collections::HashMap<&str, Vec<&ChangedSymbol>> =
        std::collections::HashMap::new();
    for sym in changed_symbols {
        name_groups.entry(sym.name.as_str()).or_default().push(sym);
    }

    for (name, group) in &name_groups {
        if group.len() < 3 {
            continue;
        }

        // Check if all instances have the same change_kind — divergence = inconsistency
        let first_kind = &group[0].change_kind;
        let outliers: Vec<String> = group
            .iter()
            .filter(|s| &s.change_kind != first_kind)
            .map(|s| format!("{} in {} ({})", s.name, s.file, s.change_kind))
            .collect();

        if !outliers.is_empty() {
            issues.push(ConsistencyIssue {
                pattern: format!("{} across {} files", name, group.len()),
                expected_count: group.len(),
                actual_count: group.len() - outliers.len(),
                outliers,
            });
        }
    }

    // Check for structural consistency: same-named symbols should have same caller count
    for (name, group) in &name_groups {
        if group.len() < 5 {
            continue;
        }

        let mut caller_counts: Vec<(String, usize)> = Vec::new();
        for sym in group {
            let escaped_name = sym.name.replace('\'', "\\'");
            let escaped_file = sym.file.replace('\'', "\\'");
            let query = format!(
                "MATCH (s:Symbol)<-[:CALLS]-(c:Symbol) \
                 WHERE s.name = '{escaped_name}' AND s.file ENDS WITH '{escaped_file}' \
                 RETURN count(c)"
            );
            let count: usize = backend
                .raw_query(&query)
                .ok()
                .and_then(|rows| rows.first()?.first()?.parse().ok())
                .unwrap_or(0);
            caller_counts.push((sym.file.clone(), count));
        }

        if caller_counts.is_empty() {
            continue;
        }

        let median_count = {
            let mut counts: Vec<usize> = caller_counts.iter().map(|(_, c)| *c).collect();
            counts.sort();
            counts[counts.len() / 2]
        };

        let structural_outliers: Vec<String> = caller_counts
            .iter()
            .filter(|(_, c)| {
                let diff = (*c).abs_diff(median_count);
                diff > 2 && median_count > 0
            })
            .map(|(file, count)| {
                format!(
                    "{} in {} ({} callers vs median {})",
                    name, file, count, median_count
                )
            })
            .collect();

        if !structural_outliers.is_empty() {
            issues.push(ConsistencyIssue {
                pattern: format!("{} caller count divergence", name),
                expected_count: group.len(),
                actual_count: group.len() - structural_outliers.len(),
                outliers: structural_outliers,
            });
        }
    }

    issues
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

/// Format the review report as Markdown.
pub fn format_review(report: &ReviewReport) -> String {
    let mut out = String::new();

    out.push_str(&format!("## PR Review: {}..HEAD\n\n", report.base_ref,));

    // Context
    out.push_str(&format!(
        "**Context:** {}\n\n",
        report.context.inferred_intent,
    ));

    // Changed symbols
    out.push_str(&format!(
        "### Changed Symbols ({})\n",
        report.changed_symbols.len(),
    ));
    if report.changed_symbols.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for sym in &report.changed_symbols {
            out.push_str(&format!(
                "  {} {} ({}) -- {}\n",
                sym.kind, sym.name, sym.file, sym.change_kind,
            ));
        }
    }
    out.push('\n');

    // Blast radius
    out.push_str(&format!(
        "### Blast Radius ({} affected)\n",
        report.blast_radius.len(),
    ));
    if report.blast_radius.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for sym in &report.blast_radius {
            out.push_str(&format!("  {} {} ({})\n", sym.kind, sym.name, sym.file,));
        }
    }
    out.push('\n');

    // Affected tests
    out.push_str(&format!(
        "### Affected Tests ({})\n",
        report.affected_tests.len(),
    ));
    if report.affected_tests.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for sym in &report.affected_tests {
            out.push_str(&format!("  {} ({})\n", sym.name, sym.file,));
        }
    }
    out.push('\n');

    // API surface changes
    out.push_str(&format!(
        "### API Surface Changes ({})\n",
        report.api_surface_changes.len(),
    ));
    if report.api_surface_changes.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for sym in &report.api_surface_changes {
            out.push_str(&format!(
                "  {} {} ({}) -- {}\n",
                sym.kind, sym.name, sym.file, sym.change_kind,
            ));
        }
    }
    out.push('\n');

    // Security findings
    out.push_str(&format!(
        "### Security Findings ({})\n",
        report.security_findings.len(),
    ));
    if report.security_findings.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for f in &report.security_findings {
            out.push_str(&format!(
                "  [{}] {}:{} -- {}\n",
                f.severity, f.file, f.line, f.message,
            ));
        }
    }
    out.push('\n');

    // Complexity hotspots
    out.push_str(&format!(
        "### Complexity Hotspots ({})\n",
        report.complexity_hotspots.len(),
    ));
    if report.complexity_hotspots.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for h in &report.complexity_hotspots {
            out.push_str(&format!(
                "  [{:>3}] {} ({})\n",
                h.complexity, h.name, h.file,
            ));
        }
    }
    out.push('\n');

    // Dead code
    out.push_str(&format!(
        "### Dead Code in Changed Files ({})\n",
        report.dead_code.len(),
    ));
    if report.dead_code.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for d in &report.dead_code {
            out.push_str(&format!("  {} {} ({})\n", d.kind, d.name, d.file,));
        }
    }
    out.push('\n');

    // Code clones
    out.push_str(&format!("### Code Clones ({})\n", report.code_clones.len(),));
    if report.code_clones.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for c in &report.code_clones {
            out.push_str(&format!(
                "  [{:.2}] {} ({}) <-> {} ({})\n",
                c.similarity, c.symbol_a, c.file_a, c.symbol_b, c.file_b,
            ));
        }
    }
    out.push('\n');

    // Consistency issues
    out.push_str(&format!(
        "### Consistency Issues ({})\n",
        report.consistency_issues.len(),
    ));
    if report.consistency_issues.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for ci in &report.consistency_issues {
            out.push_str(&format!(
                "  Pattern: {} -- {}/{} consistent\n",
                ci.pattern, ci.actual_count, ci.expected_count,
            ));
            for o in &ci.outliers {
                out.push_str(&format!("    ! {}\n", o));
            }
        }
    }
    out.push('\n');

    out
}

/// Format the review report as JSON.
pub fn format_review_json(report: &ReviewReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string())
}
