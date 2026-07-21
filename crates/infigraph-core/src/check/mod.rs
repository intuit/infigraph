//! CI check runner — configurable security, complexity, and dead-code gates.
//!
//! Load thresholds from `.infigraph/check.toml` (with sane defaults), run the
//! enabled checks, and return per-check PASS/FAIL results suitable for CI exit
//! codes and human-readable or JSON output.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::graph::GraphBackend;
use crate::security;

// ---------------------------------------------------------------------------
// Config model
// ---------------------------------------------------------------------------

/// Top-level config loaded from `.infigraph/check.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CheckConfig {
    pub security: SecurityConfig,
    pub complexity: ComplexityConfig,
    pub dead_code: DeadCodeConfig,
    pub vulnerabilities: VulnCheckConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct VulnCheckConfig {
    pub enabled: bool,
    pub max_critical: usize,
    pub max_high: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub enabled: bool,
    pub max_critical: usize,
    pub max_high: usize,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_critical: 0,
            max_high: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ComplexityConfig {
    pub enabled: bool,
    pub threshold: u32,
    pub max_violations: usize,
}

impl Default for ComplexityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 15,
            max_violations: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DeadCodeConfig {
    pub enabled: bool,
    pub max_dead: usize,
    pub ignore_patterns: Vec<String>,
}

impl Default for DeadCodeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_dead: 50,
            ignore_patterns: vec![
                "main".into(),
                "__init__".into(),
                "setUp".into(),
                "tearDown".into(),
                "Java_*".into(),
                "test_*".into(),
                "Test*".into(),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Result model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CheckStatus {
    Pass,
    Fail,
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckStatus::Pass => write!(f, "PASS"),
            CheckStatus::Fail => write!(f, "FAIL"),
        }
    }
}

/// A single named check result with summary details.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub summary: String,
    /// Human-readable detail lines (e.g. list of violations).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Load check config from a TOML file path.
/// Falls back to defaults if the file is missing.
pub fn load_config(config_path: &Path) -> Result<CheckConfig> {
    if config_path.exists() {
        let text = std::fs::read_to_string(config_path)?;
        let cfg: CheckConfig = toml::from_str(&text)?;
        Ok(cfg)
    } else {
        Ok(CheckConfig::default())
    }
}

// ---------------------------------------------------------------------------
// Check selection
// ---------------------------------------------------------------------------

/// Which checks to run.
#[derive(Debug, Clone)]
pub struct CheckSelection {
    pub security: bool,
    pub complexity: bool,
    pub dead_code: bool,
    pub vulnerabilities: bool,
}

impl CheckSelection {
    /// All checks enabled.
    pub fn all() -> Self {
        Self {
            security: true,
            complexity: true,
            dead_code: true,
            vulnerabilities: true,
        }
    }

    /// Parse a comma-separated list like "security,complexity,dead-code,vulns".
    pub fn from_csv(s: &str) -> Self {
        let mut sel = Self {
            security: false,
            complexity: false,
            dead_code: false,
            vulnerabilities: false,
        };
        for part in s.split(',') {
            match part.trim().to_lowercase().as_str() {
                "security" | "sec" => sel.security = true,
                "complexity" | "cx" => sel.complexity = true,
                "dead-code" | "dead_code" | "deadcode" => sel.dead_code = true,
                "vulnerabilities" | "vulns" | "vuln" => sel.vulnerabilities = true,
                _ => {}
            }
        }
        sel
    }
}

// ---------------------------------------------------------------------------
// Check runners
// ---------------------------------------------------------------------------

/// Run the selected checks against a project root and return results.
pub fn run_checks(
    root: &Path,
    config: &CheckConfig,
    backend: &dyn GraphBackend,
    selection: &CheckSelection,
) -> Vec<CheckResult> {
    let mut results = Vec::new();

    if selection.security && config.security.enabled {
        results.push(run_security_check(root, &config.security));
    }

    if selection.complexity && config.complexity.enabled {
        results.push(run_complexity_check(backend, &config.complexity));
    }
    if selection.dead_code && config.dead_code.enabled {
        results.push(run_dead_code_check(backend, &config.dead_code));
    }
    if selection.vulnerabilities && config.vulnerabilities.enabled {
        results.push(run_vuln_check(backend, &config.vulnerabilities));
    }

    results
}

fn run_security_check(root: &Path, cfg: &SecurityConfig) -> CheckResult {
    let canonical = match root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return CheckResult {
                name: "security".into(),
                status: CheckStatus::Fail,
                summary: format!("Failed to resolve project root: {e}"),
                details: vec![],
            };
        }
    };

    let scan = match security::scan_project(&canonical) {
        Ok(s) => s,
        Err(e) => {
            return CheckResult {
                name: "security".into(),
                status: CheckStatus::Fail,
                summary: format!("Security scan failed: {e}"),
                details: vec![],
            };
        }
    };

    let critical = scan.critical_count();
    let high = scan.high_count();
    let medium = scan.medium_count();
    let low = scan.low_count();

    let failed = critical > cfg.max_critical || high > cfg.max_high;

    let mut details = Vec::new();
    if failed {
        for f in scan.findings.iter().take(20) {
            if f.severity == security::Severity::Critical || f.severity == security::Severity::High
            {
                details.push(format!(
                    "  [{sev}] {file}:{line} -- {msg}",
                    sev = f.severity,
                    file = f.file,
                    line = f.line,
                    msg = f.message,
                ));
            }
        }
    }

    CheckResult {
        name: "security".into(),
        status: if failed {
            CheckStatus::Fail
        } else {
            CheckStatus::Pass
        },
        summary: format!(
            "{critical} critical, {high} high, {medium} medium, {low} low \
             (max_critical={}, max_high={})",
            cfg.max_critical, cfg.max_high,
        ),
        details,
    }
}

fn run_complexity_check(backend: &dyn GraphBackend, cfg: &ComplexityConfig) -> CheckResult {
    let query = format!(
        "MATCH (s:Symbol) WHERE s.complexity >= {} \
         AND (s.kind = 'Function' OR s.kind = 'Method') \
         RETURN s.name, s.file, s.complexity ORDER BY s.complexity DESC",
        cfg.threshold,
    );

    let rows = match backend.raw_query(&query) {
        Ok(r) => r,
        Err(e) => {
            return CheckResult {
                name: "complexity".into(),
                status: CheckStatus::Fail,
                summary: format!("Query failed: {e}"),
                details: vec![],
            };
        }
    };

    let count = rows.len();
    let failed = count > cfg.max_violations;

    let details: Vec<String> = if failed {
        rows.iter()
            .take(20)
            .filter_map(|row| {
                let name = row.first()?;
                let file = row.get(1)?;
                let cplx = row.get(2)?;
                Some(format!("  [{cplx:>3}] {name}  ({file})"))
            })
            .collect()
    } else {
        vec![]
    };

    CheckResult {
        name: "complexity".into(),
        status: if failed {
            CheckStatus::Fail
        } else {
            CheckStatus::Pass
        },
        summary: format!(
            "{count} symbols >= threshold {threshold} (max_violations={max})",
            threshold = cfg.threshold,
            max = cfg.max_violations,
        ),
        details,
    }
}

fn run_dead_code_check(backend: &dyn GraphBackend, cfg: &DeadCodeConfig) -> CheckResult {
    let query = "MATCH (s:Symbol) WHERE s.kind IN ['Function', 'Method'] \
                 AND NOT EXISTS { MATCH ()-[:CALLS]->(s) } \
                 AND NOT EXISTS { MATCH (p:Symbol)<-[:INHERITS]-() WHERE p.file = s.file AND p.kind IN ['Class', 'Interface', 'Trait'] } \
                 RETURN s.name, s.kind, s.file ORDER BY s.file, s.name";

    let rows = match backend.raw_query(query) {
        Ok(r) => r,
        Err(e) => {
            return CheckResult {
                name: "dead-code".into(),
                status: CheckStatus::Fail,
                summary: format!("Query failed: {e}"),
                details: vec![],
            };
        }
    };

    // Filter out ignored patterns (supports exact match and prefix glob with trailing *)
    let dead: Vec<&Vec<String>> = rows
        .iter()
        .filter(|row| {
            let name = row.first().map(|s| s.as_str()).unwrap_or("");
            !cfg.ignore_patterns.iter().any(|pat| {
                if let Some(prefix) = pat.strip_suffix('*') {
                    name.starts_with(prefix)
                } else {
                    name == pat
                }
            })
        })
        .collect();

    let count = dead.len();
    let failed = count > cfg.max_dead;

    let details: Vec<String> = if failed {
        dead.iter()
            .take(20)
            .filter_map(|row| {
                let name = row.first()?;
                let kind = row.get(1)?;
                let file = row.get(2)?;
                Some(format!("  {kind:>8} {name}  ({file})"))
            })
            .collect()
    } else {
        vec![]
    };

    CheckResult {
        name: "dead-code".into(),
        status: if failed {
            CheckStatus::Fail
        } else {
            CheckStatus::Pass
        },
        summary: format!("{count} dead symbols (max_dead={max})", max = cfg.max_dead),
        details,
    }
}

fn run_vuln_check(backend: &dyn GraphBackend, cfg: &VulnCheckConfig) -> CheckResult {
    let deps = match crate::manifest::query_deps(backend) {
        Ok(d) => d,
        Err(e) => {
            return CheckResult {
                name: "vulns".into(),
                status: CheckStatus::Pass,
                summary: format!("failed to query deps: {e}"),
                details: vec![],
            };
        }
    };
    if deps.is_empty() {
        return CheckResult {
            name: "vulns".into(),
            status: CheckStatus::Pass,
            summary: "no dependencies indexed (run infigraph index-manifests first)".into(),
            details: vec![],
        };
    }

    let report = match crate::vuln::scan_deps(&deps) {
        Ok(r) => r,
        Err(e) => {
            return CheckResult {
                name: "vulns".into(),
                status: CheckStatus::Pass,
                summary: format!("scan skipped: {e}"),
                details: vec![],
            };
        }
    };

    let critical = report
        .findings
        .iter()
        .filter(|f| f.severity == "CRITICAL")
        .count();
    let high = report
        .findings
        .iter()
        .filter(|f| f.severity == "HIGH")
        .count();
    let medium = report
        .findings
        .iter()
        .filter(|f| f.severity == "MEDIUM")
        .count();
    let low = report
        .findings
        .iter()
        .filter(|f| f.severity == "LOW")
        .count();

    let failed = critical > cfg.max_critical || high > cfg.max_high;

    let details: Vec<String> = if failed {
        report
            .findings
            .iter()
            .filter(|f| f.severity == "CRITICAL" || f.severity == "HIGH")
            .take(20)
            .map(|f| {
                format!(
                    "  [{}] {} {} -- {}",
                    f.severity, f.dep_name, f.dep_version, f.summary
                )
            })
            .collect()
    } else {
        vec![]
    };

    CheckResult {
        name: "vulns".into(),
        status: if failed { CheckStatus::Fail } else { CheckStatus::Pass },
        summary: format!(
            "{critical} critical, {high} high, {medium} medium, {low} low (max_critical={}, max_high={})",
            cfg.max_critical, cfg.max_high,
        ),
        details,
    }
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

/// Format results as a human-readable table.
pub fn format_table(results: &[CheckResult]) -> String {
    let mut out = String::new();

    out.push_str("\n  Check         Status   Summary\n");
    out.push_str("  ------------- ------   -------\n");

    for r in results {
        let status_str = match r.status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Fail => "FAIL",
        };
        out.push_str(&format!(
            "  {:<13} {:<8} {}\n",
            r.name, status_str, r.summary
        ));
    }

    // Print details for failures.
    let failures: Vec<_> = results
        .iter()
        .filter(|r| r.status == CheckStatus::Fail)
        .collect();
    if !failures.is_empty() {
        out.push('\n');
        for r in &failures {
            if !r.details.is_empty() {
                out.push_str(&format!("  {} details:\n", r.name));
                for d in &r.details {
                    out.push_str(&format!("{d}\n"));
                }
                out.push('\n');
            }
        }
    }

    let total = results.len();
    let passed = results
        .iter()
        .filter(|r| r.status == CheckStatus::Pass)
        .count();
    let failed_count = total - passed;

    out.push_str(&format!("\n  {passed}/{total} checks passed"));
    if failed_count > 0 {
        out.push_str(&format!(", {failed_count} failed"));
    }
    out.push('\n');

    out
}

/// Format results as JSON.
pub fn format_json(results: &[CheckResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}
