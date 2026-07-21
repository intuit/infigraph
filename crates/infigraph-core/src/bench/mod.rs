use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::graph::GraphBackend;

/// Quality metrics snapshot captured from the code graph and security scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityMetrics {
    pub timestamp: u64,
    pub symbols: usize,
    pub modules: usize,
    pub calls_edges: usize,
    pub inherits_edges: usize,
    pub dead_code_count: usize,
    pub security_critical: usize,
    pub security_high: usize,
    pub security_medium: usize,
    pub security_low: usize,
}

/// Stored baseline with project path metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityBaseline {
    pub metrics: QualityMetrics,
    pub project_path: String,
}

impl QualityMetrics {
    /// Capture current quality metrics from the graph store and security scanner.
    pub fn capture(root: &Path, backend: &dyn GraphBackend) -> Result<Self> {
        let symbols = count_query(backend, "MATCH (s:Symbol) RETURN count(s)");
        let modules = count_query(backend, "MATCH (m:Module) RETURN count(m)");
        let calls_edges = count_query(backend, "MATCH ()-[r:CALLS]->() RETURN count(r)");
        let inherits_edges = count_query(backend, "MATCH ()-[r:INHERITS]->() RETURN count(r)");

        let dead_rows = backend
            .raw_query(
                "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] \
                 AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } RETURN count(s)",
            )
            .unwrap_or_default();
        let dead_code_count = dead_rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);

        let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let (critical, high, medium, low) = match crate::security::scan_project(&canonical) {
            Ok(scan) => (
                scan.critical_count(),
                scan.high_count(),
                scan.medium_count(),
                scan.low_count(),
            ),
            Err(_) => (0, 0, 0, 0),
        };

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Self {
            timestamp,
            symbols,
            modules,
            calls_edges,
            inherits_edges,
            dead_code_count,
            security_critical: critical,
            security_high: high,
            security_medium: medium,
            security_low: low,
        })
    }

    pub fn format(&self) -> String {
        let mut out = String::new();
        out.push_str("\n  Metric              Value\n");
        out.push_str("  ------------------  --------\n");
        out.push_str(&format!("  symbols             {}\n", self.symbols));
        out.push_str(&format!("  modules             {}\n", self.modules));
        out.push_str(&format!("  calls_edges         {}\n", self.calls_edges));
        out.push_str(&format!("  inherits_edges      {}\n", self.inherits_edges));
        out.push_str(&format!("  dead_code           {}\n", self.dead_code_count));
        out.push_str(&format!(
            "  security_critical   {}\n",
            self.security_critical
        ));
        out.push_str(&format!("  security_high       {}\n", self.security_high));
        out.push_str(&format!("  security_medium     {}\n", self.security_medium));
        out.push_str(&format!("  security_low        {}\n", self.security_low));
        out
    }
}

fn count_query(backend: &dyn GraphBackend, cypher: &str) -> usize {
    backend
        .raw_query(cypher)
        .ok()
        .and_then(|rows| rows.first().cloned())
        .and_then(|row| row.first().cloned())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
}

pub fn load_baseline(root: &Path) -> Option<QualityBaseline> {
    let path = root.join(".infigraph").join("quality_baseline.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_baseline(root: &Path, metrics: &QualityMetrics) -> Result<()> {
    let baseline = QualityBaseline {
        metrics: metrics.clone(),
        project_path: root.to_string_lossy().to_string(),
    };
    let path = root.join(".infigraph").join("quality_baseline.json");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let json = serde_json::to_string_pretty(&baseline)?;
    std::fs::write(&path, json)?;
    Ok(())
}

pub struct ComparisonResult {
    pub metric: String,
    pub baseline: String,
    pub current: String,
    pub change: String,
    pub regression: bool,
}

pub fn compare(baseline: &QualityMetrics, current: &QualityMetrics) -> Vec<ComparisonResult> {
    vec![
        compare_metric("symbols", baseline.symbols, current.symbols, false),
        compare_metric("modules", baseline.modules, current.modules, false),
        compare_metric(
            "calls_edges",
            baseline.calls_edges,
            current.calls_edges,
            false,
        ),
        compare_metric(
            "inherits_edges",
            baseline.inherits_edges,
            current.inherits_edges,
            false,
        ),
        compare_metric(
            "dead_code",
            baseline.dead_code_count,
            current.dead_code_count,
            true,
        ),
        compare_metric(
            "security_critical",
            baseline.security_critical,
            current.security_critical,
            true,
        ),
        compare_metric(
            "security_high",
            baseline.security_high,
            current.security_high,
            true,
        ),
        compare_metric(
            "security_medium",
            baseline.security_medium,
            current.security_medium,
            true,
        ),
        compare_metric(
            "security_low",
            baseline.security_low,
            current.security_low,
            true,
        ),
    ]
}

fn compare_metric(
    name: &str,
    baseline: usize,
    current: usize,
    higher_is_worse: bool,
) -> ComparisonResult {
    let change = if baseline == 0 {
        if current == 0 {
            "same".to_string()
        } else {
            format!("+{current}")
        }
    } else {
        let pct = ((current as f64 - baseline as f64) / baseline as f64 * 100.0) as i64;
        if pct == 0 {
            "same".to_string()
        } else if pct > 0 {
            format!("+{pct}%")
        } else {
            format!("{pct}%")
        }
    };

    let regression = if higher_is_worse {
        current > baseline && (current as f64 > baseline as f64 * 1.02)
    } else {
        current < baseline && ((current as f64) < baseline as f64 * 0.98)
    };

    ComparisonResult {
        metric: name.to_string(),
        baseline: baseline.to_string(),
        current: current.to_string(),
        change,
        regression,
    }
}

pub fn format_comparison(results: &[ComparisonResult]) -> String {
    let mut out = String::new();
    out.push_str("\n  Metric              Baseline   Current    Change\n");
    out.push_str("  ------------------  --------   -------    ------\n");
    for r in results {
        let flag = if r.regression { " REGRESSION" } else { "" };
        out.push_str(&format!(
            "  {:<18}  {:>8}   {:>7}    {}{}\n",
            r.metric, r.baseline, r.current, r.change, flag
        ));
    }
    out
}
