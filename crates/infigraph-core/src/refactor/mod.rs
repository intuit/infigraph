use std::collections::HashMap;

use anyhow::Result;
use rayon::prelude::*;

use crate::embed;
use crate::graph::GraphBackend;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Category {
    SplitFile,
    ExtractFunction,
    MergeDuplicates,
    RemoveDeadCode,
    ReduceCoupling,
    SimplifyLogic,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SplitFile => write!(f, "split_file"),
            Self::ExtractFunction => write!(f, "extract_function"),
            Self::MergeDuplicates => write!(f, "merge_duplicates"),
            Self::RemoveDeadCode => write!(f, "remove_dead_code"),
            Self::ReduceCoupling => write!(f, "reduce_coupling"),
            Self::SimplifyLogic => write!(f, "simplify_logic"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Impact {
    High,
    Medium,
    Low,
}

impl Impact {
    fn score(&self) -> u32 {
        match self {
            Self::High => 3,
            Self::Medium => 2,
            Self::Low => 1,
        }
    }
}

impl std::fmt::Display for Impact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effort {
    High,
    Medium,
    Low,
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::High => write!(f, "high"),
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
        }
    }
}

impl Effort {
    fn score(&self) -> u32 {
        match self {
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Recommendation {
    pub category: Category,
    pub target: String,
    pub impact: Impact,
    pub effort: Effort,
    pub rationale: String,
}

impl Recommendation {
    fn priority(&self) -> u32 {
        self.impact.score() * self.effort.score()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    All,
    Complexity,
    Duplication,
    Coupling,
    Size,
}

impl Focus {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "complexity" => Self::Complexity,
            "duplication" | "clones" => Self::Duplication,
            "coupling" => Self::Coupling,
            "size" => Self::Size,
            _ => Self::All,
        }
    }
}

struct SymbolInfo {
    id: String,
    name: String,
    kind: String,
    file: String,
    complexity: u32,
    start_line: u32,
    end_line: u32,
}

pub fn analyze(
    backend: &dyn GraphBackend,
    embeddings_path: Option<&std::path::Path>,
    target: Option<&str>,
    focus: Focus,
    limit: usize,
) -> Result<Vec<Recommendation>> {
    let symbols = load_symbols(backend, target)?;

    if symbols.is_empty() {
        return Ok(vec![]);
    }

    let mut recommendations = Vec::new();

    let run_all = focus == Focus::All;

    if run_all || focus == Focus::Size {
        analyze_file_sizes(&symbols, &mut recommendations);
    }

    if run_all || focus == Focus::Complexity {
        analyze_complexity(&symbols, &mut recommendations);
    }

    if run_all || focus == Focus::Coupling {
        analyze_coupling(backend, &symbols, &mut recommendations)?;
    }

    if run_all || focus == Focus::Duplication {
        analyze_duplication(backend, &symbols, embeddings_path, &mut recommendations)?;
    }

    if run_all {
        analyze_dead_code(backend, &symbols, &mut recommendations)?;
    }

    recommendations.sort_by_key(|r| std::cmp::Reverse(r.priority()));
    recommendations.truncate(limit);

    Ok(recommendations)
}

fn load_symbols(backend: &dyn GraphBackend, target: Option<&str>) -> Result<Vec<SymbolInfo>> {
    let query = if let Some(t) = target {
        format!(
            "MATCH (s:Symbol) WHERE s.file CONTAINS '{}' RETURN s.id, s.name, s.kind, s.file, s.complexity, s.start_line, s.end_line ORDER BY s.file, s.start_line",
            t.replace('\'', "\\'")
        )
    } else {
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method', 'Class', 'Struct', 'Interface', 'Test'] RETURN s.id, s.name, s.kind, s.file, s.complexity, s.start_line, s.end_line ORDER BY s.file, s.start_line".to_string()
    };

    let rows = backend.raw_query(&query)?;
    Ok(rows
        .into_iter()
        .map(|r| SymbolInfo {
            id: r[0].clone(),
            name: r[1].clone(),
            kind: r[2].clone(),
            file: r[3].clone(),
            complexity: r.get(4).and_then(|v| v.parse().ok()).unwrap_or(0),
            start_line: r.get(5).and_then(|v| v.parse().ok()).unwrap_or(0),
            end_line: r.get(6).and_then(|v| v.parse().ok()).unwrap_or(0),
        })
        .collect())
}

fn analyze_file_sizes(symbols: &[SymbolInfo], recs: &mut Vec<Recommendation>) {
    let mut file_stats: HashMap<&str, (usize, u32)> = HashMap::new();

    for sym in symbols {
        let entry = file_stats.entry(sym.file.as_str()).or_insert((0, 0));
        entry.0 += 1;
        if sym.end_line > entry.1 {
            entry.1 = sym.end_line;
        }
    }

    for (file, (symbol_count, max_line)) in &file_stats {
        if *max_line > 1000 || *symbol_count > 40 {
            let impact = if *max_line > 2000 || *symbol_count > 80 {
                Impact::High
            } else {
                Impact::Medium
            };
            let effort = if *symbol_count > 60 {
                Effort::High
            } else {
                Effort::Medium
            };
            recs.push(Recommendation {
                category: Category::SplitFile,
                target: file.to_string(),
                impact,
                effort,
                rationale: format!(
                    "{} lines, {} symbols. Consider splitting into focused modules.",
                    max_line, symbol_count
                ),
            });
        }
    }
}

fn analyze_complexity(symbols: &[SymbolInfo], recs: &mut Vec<Recommendation>) {
    let threshold = 15u32;
    let mut hotspots: Vec<&SymbolInfo> = symbols
        .iter()
        .filter(|s| {
            s.complexity >= threshold
                && (s.kind == "Function" || s.kind == "Method" || s.kind == "Test")
        })
        .collect();

    hotspots.sort_by_key(|s| std::cmp::Reverse(s.complexity));

    for sym in hotspots.iter().take(10) {
        let loc = sym.end_line.saturating_sub(sym.start_line);
        let (impact, effort) = if sym.complexity > 30 {
            (Impact::High, Effort::High)
        } else if sym.complexity > 20 {
            (Impact::High, Effort::Medium)
        } else {
            (Impact::Medium, Effort::Medium)
        };

        let category = if loc > 80 {
            Category::ExtractFunction
        } else {
            Category::SimplifyLogic
        };

        recs.push(Recommendation {
            category,
            target: format!("{} ({}:{})", sym.name, sym.file, sym.start_line),
            impact,
            effort,
            rationale: format!(
                "Cyclomatic complexity {}. {} lines. Break into smaller functions or simplify branching.",
                sym.complexity, loc
            ),
        });
    }
}

fn analyze_coupling(
    backend: &dyn GraphBackend,
    symbols: &[SymbolInfo],
    recs: &mut Vec<Recommendation>,
) -> Result<()> {
    let callable_ids: Vec<&str> = symbols
        .iter()
        .filter(|s| s.kind == "Function" || s.kind == "Method")
        .map(|s| s.id.as_str())
        .collect();

    if callable_ids.is_empty() {
        return Ok(());
    }

    let fan_out_query = "MATCH (s:Symbol)-[:CALLS]->(t:Symbol) WHERE s.kind IN ['Function', 'Method'] RETURN s.id, count(DISTINCT t) ORDER BY count(DISTINCT t) DESC";
    let fan_out_rows = backend.raw_query(fan_out_query)?;

    let fan_in_query = "MATCH (s:Symbol)<-[:CALLS]-(t:Symbol) WHERE s.kind IN ['Function', 'Method'] RETURN s.id, count(DISTINCT t) ORDER BY count(DISTINCT t) DESC";
    let fan_in_rows = backend.raw_query(fan_in_query)?;

    let sym_lookup: HashMap<&str, &SymbolInfo> =
        symbols.iter().map(|s| (s.id.as_str(), s)).collect();

    for row in fan_out_rows.iter().take(20) {
        let count: u32 = row.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        if count < 15 {
            continue;
        }
        let id = &row[0];
        if let Some(sym) = sym_lookup.get(id.as_str()) {
            let impact = if count > 25 {
                Impact::High
            } else {
                Impact::Medium
            };
            recs.push(Recommendation {
                category: Category::ReduceCoupling,
                target: format!("{} ({}:{})", sym.name, sym.file, sym.start_line),
                impact,
                effort: Effort::Medium,
                rationale: format!(
                    "Fan-out of {} — calls {} distinct functions. High coupling makes changes risky.",
                    count, count
                ),
            });
        }
    }

    for row in fan_in_rows.iter().take(20) {
        let count: u32 = row.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        if count < 20 {
            continue;
        }
        let id = &row[0];
        if let Some(sym) = sym_lookup.get(id.as_str()) {
            recs.push(Recommendation {
                category: Category::ReduceCoupling,
                target: format!("{} ({}:{})", sym.name, sym.file, sym.start_line),
                impact: Impact::High,
                effort: Effort::High,
                rationale: format!(
                    "Fan-in of {} — {} callers depend on this. Changes have wide blast radius. Consider interface extraction.",
                    count, count
                ),
            });
        }
    }

    Ok(())
}

fn analyze_duplication(
    _backend: &dyn GraphBackend,
    symbols: &[SymbolInfo],
    embeddings_path: Option<&std::path::Path>,
    recs: &mut Vec<Recommendation>,
) -> Result<()> {
    let callables: Vec<&SymbolInfo> = symbols
        .iter()
        .filter(|s| s.kind == "Function" || s.kind == "Method")
        .collect();

    if callables.len() < 2 {
        return Ok(());
    }

    let embedder = embed::best_embedder();

    let cached: HashMap<String, Vec<f32>> = if let Some(path) = embeddings_path {
        if path.exists() {
            embed::load_embeddings_cached(path)?.into_iter().collect()
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };

    let sym_vecs: Vec<(&SymbolInfo, Vec<f32>)> = callables
        .par_iter()
        .map(|sym| {
            let emb = cached
                .get(&sym.id)
                .cloned()
                .unwrap_or_else(|| embedder.embed(&sym.name).unwrap_or_default());
            (*sym, emb)
        })
        .filter(|(_, emb)| !emb.is_empty())
        .collect();

    let threshold = 0.90f32;
    let n = sym_vecs.len();
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();

    for i in 0..n {
        for j in (i + 1)..n {
            if sym_vecs[i].0.file == sym_vecs[j].0.file {
                continue;
            }
            let sim = embed::cosine_similarity(&sym_vecs[i].1, &sym_vecs[j].1);
            if sim >= threshold {
                pairs.push((sim, i, j));
            }
        }
    }

    pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    for (sim, i, j) in pairs.iter().take(5) {
        let (impact, effort) = if *sim > 0.95 {
            (Impact::High, Effort::Low)
        } else {
            (Impact::Medium, Effort::Low)
        };
        recs.push(Recommendation {
            category: Category::MergeDuplicates,
            target: format!("{} ↔ {}", sym_vecs[*i].0.name, sym_vecs[*j].0.name),
            impact,
            effort,
            rationale: format!(
                "{:.0}% similar. {} ({}) and {} ({}). Extract shared logic.",
                sim * 100.0,
                sym_vecs[*i].0.name,
                sym_vecs[*i].0.file,
                sym_vecs[*j].0.name,
                sym_vecs[*j].0.file,
            ),
        });
    }

    Ok(())
}

fn analyze_dead_code(
    backend: &dyn GraphBackend,
    symbols: &[SymbolInfo],
    recs: &mut Vec<Recommendation>,
) -> Result<()> {
    let rows = backend.raw_query(
        "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN s.id, s.name, s.file",
    )?;

    let entry_points = ["main", "__init__", "setUp", "tearDown", "setup", "teardown"];
    let target_files: HashMap<&str, bool> =
        symbols.iter().map(|s| (s.file.as_str(), true)).collect();

    let dead: Vec<&Vec<String>> = rows
        .iter()
        .filter(|row| {
            !entry_points.contains(&row[1].as_str())
                && !row[1].starts_with("test_")
                && !row[1].starts_with("Test")
                && target_files.contains_key(row[2].as_str())
        })
        .collect();

    if dead.is_empty() {
        return Ok(());
    }

    let mut by_file: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in &dead {
        by_file
            .entry(row[2].as_str())
            .or_default()
            .push(row[1].as_str());
    }

    for (file, names) in &by_file {
        if names.len() >= 3 {
            recs.push(Recommendation {
                category: Category::RemoveDeadCode,
                target: file.to_string(),
                impact: Impact::Low,
                effort: Effort::Low,
                rationale: format!(
                    "{} unreachable functions: {}. Safe to remove (verify no dynamic dispatch).",
                    names.len(),
                    names.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                ),
            });
        } else {
            for name in names {
                recs.push(Recommendation {
                    category: Category::RemoveDeadCode,
                    target: format!("{} ({})", name, file),
                    impact: Impact::Low,
                    effort: Effort::Low,
                    rationale: "Zero callers. Safe to remove (verify no dynamic dispatch)."
                        .to_string(),
                });
            }
        }
    }

    Ok(())
}

pub fn format_recommendations(recs: &[Recommendation], target: Option<&str>) -> String {
    if recs.is_empty() {
        return format!(
            "No refactoring recommendations for {}.",
            target.unwrap_or("project")
        );
    }

    let mut out = format!(
        "Refactoring Analysis: {}\n{} recommendations, sorted by impact/effort ratio\n\n",
        target.unwrap_or("project"),
        recs.len()
    );

    let mut current_impact = None;

    for (i, rec) in recs.iter().enumerate() {
        let impact_label = format!("{} IMPACT", rec.impact).to_uppercase();
        if current_impact.as_ref() != Some(&impact_label) {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&impact_label);
            out.push_str(":\n");
            current_impact = Some(impact_label);
        }

        out.push_str(&format!(
            "{}. [{}] {}\n   Rationale: {}\n   Effort: {} | Impact: {}\n\n",
            i + 1,
            rec.category,
            rec.target,
            rec.rationale,
            rec.effort,
            rec.impact,
        ));
    }

    out
}
