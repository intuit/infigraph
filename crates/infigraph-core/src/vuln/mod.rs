//! Dependency vulnerability scanning via the OSV (Open Source Vulnerabilities) API.
//!
//! Cross-references project dependencies (from `manifest::query_deps`) against
//! the OSV batch endpoint and reports known vulnerabilities with severity.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::manifest::DepEntry;

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct VulnEntry {
    pub dep_name: String,
    pub dep_version: String,
    pub ecosystem: String,
    pub vuln_id: String,
    pub summary: String,
    pub severity: String,
    pub fixed_version: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VulnReport {
    pub total_deps: usize,
    pub vulnerable_deps: usize,
    pub findings: Vec<VulnEntry>,
}

// ── OSV request/response models (serde) ─────────────────────────────────────

#[derive(Debug, Serialize)]
struct OsvBatchRequest {
    queries: Vec<OsvQuery>,
}

#[derive(Debug, Serialize)]
struct OsvQuery {
    package: OsvPackage,
    version: String,
}

#[derive(Debug, Serialize)]
struct OsvPackage {
    name: String,
    ecosystem: String,
}

#[derive(Debug, Deserialize)]
struct OsvBatchResponse {
    results: Vec<OsvResultEntry>,
}

#[derive(Debug, Deserialize)]
struct OsvResultEntry {
    vulns: Option<Vec<OsvVuln>>,
}

#[derive(Debug, Deserialize)]
struct OsvVuln {
    id: String,
    summary: Option<String>,
    severity: Option<Vec<OsvSeverity>>,
    affected: Option<Vec<OsvAffected>>,
    references: Option<Vec<OsvReference>>,
    database_specific: Option<OsvDatabaseSpecific>,
}

#[derive(Debug, Deserialize)]
struct OsvSeverity {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    severity_type: Option<String>,
    score: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OsvAffected {
    ranges: Option<Vec<OsvRange>>,
}

#[derive(Debug, Deserialize)]
struct OsvRange {
    events: Option<Vec<OsvEvent>>,
}

#[derive(Debug, Deserialize)]
struct OsvEvent {
    fixed: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OsvReference {
    #[serde(rename = "type")]
    ref_type: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OsvDatabaseSpecific {
    severity: Option<String>,
}

// ── Ecosystem mapping ───────────────────────────────────────────────────────

/// Map ecosystem names to OSV ecosystem names.
fn map_ecosystem(eco: &str) -> &str {
    match eco.to_lowercase().as_str() {
        "npm" => "npm",
        "cargo" => "crates.io",
        "pip" | "pypi" => "PyPI",
        "maven" | "gradle" => "Maven",
        "gem" => "RubyGems",
        "nuget" => "NuGet",
        "go" => "Go",
        "composer" => "Packagist",
        "pub" => "Pub",
        _other => {
            // Return the input as-is for unknown ecosystems.
            // We can't return `_other` directly because it borrows the
            // lowercased temporary. Use the original eco instead.
            eco
        }
    }
}

// ── CVSS score extraction ───────────────────────────────────────────────────

/// Extract a severity label from a CVSS v3 vector string or numeric score.
fn severity_from_cvss(score_str: &str) -> &'static str {
    // Some OSV entries include the numeric base score directly (e.g. "9.8")
    if let Ok(base) = score_str.parse::<f64>() {
        return cvss_to_label(base);
    }
    // Try to estimate from a CVSS vector string
    if score_str.starts_with("CVSS:") {
        // Count high-impact metrics as a rough proxy for base score
        let high_count = score_str.matches(":H").count();
        let none_count = score_str.matches(":N").count();
        let rough = match high_count {
            0..=1 => 4.0,
            2..=3 => 7.0,
            _ => 9.0,
        };
        let bump = (none_count as f64) * 0.5;
        return cvss_to_label(rough + bump);
    }
    "UNKNOWN"
}

fn cvss_to_label(base: f64) -> &'static str {
    if base >= 9.0 {
        "CRITICAL"
    } else if base >= 7.0 {
        "HIGH"
    } else if base >= 4.0 {
        "MEDIUM"
    } else {
        "LOW"
    }
}

/// Determine severity label for a single OSV vuln entry.
fn extract_severity(vuln: &OsvVuln) -> String {
    // 1. Try CVSS scores from the severity array
    if let Some(ref sev_list) = vuln.severity {
        for s in sev_list {
            if let Some(ref score) = s.score {
                let label = severity_from_cvss(score);
                if label != "UNKNOWN" {
                    return label.to_string();
                }
            }
        }
    }
    // 2. Try database_specific.severity
    if let Some(ref db) = vuln.database_specific {
        if let Some(ref sev) = db.severity {
            return sev.to_uppercase();
        }
    }
    "UNKNOWN".to_string()
}

/// Extract the first fixed version from affected ranges.
fn extract_fixed_version(vuln: &OsvVuln) -> Option<String> {
    if let Some(ref affected) = vuln.affected {
        for a in affected {
            if let Some(ref ranges) = a.ranges {
                for r in ranges {
                    if let Some(ref events) = r.events {
                        for e in events {
                            if let Some(ref fixed) = e.fixed {
                                return Some(fixed.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract the best advisory URL from references.
fn extract_url(vuln: &OsvVuln) -> String {
    if let Some(ref refs) = vuln.references {
        // Prefer ADVISORY type
        for r in refs {
            if r.ref_type.as_deref() == Some("ADVISORY") {
                if let Some(ref url) = r.url {
                    return url.clone();
                }
            }
        }
        // Fall back to first URL
        for r in refs {
            if let Some(ref url) = r.url {
                return url.clone();
            }
        }
    }
    format!("https://osv.dev/vulnerability/{}", vuln.id)
}

// ── Version cleaning ────────────────────────────────────────────────────────

/// Strip semver operators (^, ~, >=, <=, >, <, =) from version strings.
fn clean_version(version: &str) -> &str {
    version.trim_start_matches(|c: char| !c.is_ascii_digit())
}

/// Returns true if the version is usable for OSV queries.
fn is_valid_version(version: &str) -> bool {
    let cleaned = clean_version(version);
    !cleaned.is_empty() && cleaned != "*"
}

// ── OSV API client ──────────────────────────────────────────────────────────

const OSV_BATCH_URL: &str = "https://api.osv.dev/v1/querybatch";
const OSV_BATCH_SIZE: usize = 1000;

/// Send a batch of queries to the OSV API and return per-query results.
fn query_osv_batch(queries: &[OsvQuery]) -> Result<Vec<OsvResultEntry>> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }

    let body = OsvBatchRequest {
        queries: queries.iter().map(|q| OsvQuery {
            package: OsvPackage {
                name: q.package.name.clone(),
                ecosystem: q.package.ecosystem.clone(),
            },
            version: q.version.clone(),
        }).collect(),
    };

    let body_json = serde_json::to_string(&body)?;

    let resp = ureq::post(OSV_BATCH_URL)
        .set("Content-Type", "application/json")
        .send_string(&body_json);

    match resp {
        Ok(response) => {
            let text = response.into_string()?;
            let batch_resp: OsvBatchResponse = serde_json::from_str(&text)?;
            Ok(batch_resp.results)
        }
        Err(e) => {
            eprintln!("Warning: OSV API request failed: {e}");
            // Return empty results for each query so indexing is preserved
            Ok(queries.iter().map(|_| OsvResultEntry { vulns: None }).collect())
        }
    }
}

// ── Main scan entry point ───────────────────────────────────────────────────

/// Scan a list of dependencies against the OSV vulnerability database.
///
/// Maps ecosystem names, batches queries (up to 1000 per request), parses
/// responses, and returns a structured report.
pub fn scan_deps(deps: &[DepEntry]) -> Result<VulnReport> {
    // Build queries, filtering out deps with unusable versions
    let valid_deps: Vec<&DepEntry> = deps
        .iter()
        .filter(|d| is_valid_version(&d.version))
        .collect();

    let queries: Vec<OsvQuery> = valid_deps
        .iter()
        .map(|d| OsvQuery {
            package: OsvPackage {
                name: d.name.clone(),
                ecosystem: map_ecosystem(&d.ecosystem).to_string(),
            },
            version: clean_version(&d.version).to_string(),
        })
        .collect();

    // Send in batches
    let mut all_results: Vec<OsvResultEntry> = Vec::with_capacity(queries.len());
    for chunk in queries.chunks(OSV_BATCH_SIZE) {
        let batch_results = query_osv_batch(chunk)?;
        all_results.extend(batch_results);
    }

    // Parse results
    let mut findings = Vec::new();
    let mut vulnerable_dep_names = std::collections::HashSet::new();

    for (i, result) in all_results.iter().enumerate() {
        if i >= valid_deps.len() {
            break;
        }
        let dep = valid_deps[i];

        if let Some(ref vulns) = result.vulns {
            for vuln in vulns {
                vulnerable_dep_names.insert(format!("{}@{}", dep.name, dep.version));
                findings.push(VulnEntry {
                    dep_name: dep.name.clone(),
                    dep_version: clean_version(&dep.version).to_string(),
                    ecosystem: dep.ecosystem.clone(),
                    vuln_id: vuln.id.clone(),
                    summary: vuln.summary.clone().unwrap_or_default(),
                    severity: extract_severity(vuln),
                    fixed_version: extract_fixed_version(vuln),
                    url: extract_url(vuln),
                });
            }
        }
    }

    // Sort findings: CRITICAL first, then HIGH, MEDIUM, LOW, UNKNOWN
    findings.sort_by(|a, b| {
        severity_rank(&a.severity).cmp(&severity_rank(&b.severity))
            .then(a.dep_name.cmp(&b.dep_name))
    });

    Ok(VulnReport {
        total_deps: deps.len(),
        vulnerable_deps: vulnerable_dep_names.len(),
        findings,
    })
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "CRITICAL" => 0,
        "HIGH" => 1,
        "MEDIUM" => 2,
        "LOW" => 3,
        _ => 4,
    }
}

/// Filter report findings by minimum severity.
pub fn filter_by_severity(report: &mut VulnReport, min_severity: &str) {
    let min_rank = severity_rank(&min_severity.to_uppercase());
    report.findings.retain(|f| severity_rank(&f.severity) <= min_rank);
    let mut names = std::collections::HashSet::new();
    for f in &report.findings {
        names.insert(format!("{}@{}", f.dep_name, f.dep_version));
    }
    report.vulnerable_deps = names.len();
}

/// Filter report findings by ecosystem.
pub fn filter_by_ecosystem(report: &mut VulnReport, ecosystem: &str) {
    report.findings.retain(|f| f.ecosystem.eq_ignore_ascii_case(ecosystem));
    let mut names = std::collections::HashSet::new();
    for f in &report.findings {
        names.insert(format!("{}@{}", f.dep_name, f.dep_version));
    }
    report.vulnerable_deps = names.len();
}

/// Format the report as a human-readable table.
pub fn format_table(report: &VulnReport) -> String {
    if report.findings.is_empty() {
        return format!(
            "Vulnerability Scan Results\n\n  No vulnerabilities found ({} dependencies scanned)\n",
            report.total_deps
        );
    }

    let mut out = String::from("Vulnerability Scan Results\n\n");

    // Header
    out.push_str(&format!(
        "  {:<20} {:<12} {:<18} {:<10} {}\n",
        "Dep", "Version", "Vuln ID", "Severity", "Summary"
    ));

    // Findings
    for f in &report.findings {
        let summary_truncated = if f.summary.len() > 60 {
            format!("{}...", &f.summary[..57])
        } else {
            f.summary.clone()
        };
        out.push_str(&format!(
            "  {:<20} {:<12} {:<18} {:<10} {}\n",
            truncate_str(&f.dep_name, 20),
            truncate_str(&f.dep_version, 12),
            truncate_str(&f.vuln_id, 18),
            &f.severity,
            summary_truncated,
        ));
    }

    out.push_str(&format!(
        "\n  {} vulnerable dependencies found (out of {} scanned)\n",
        report.vulnerable_deps, report.total_deps
    ));

    out
}

/// Format the report as JSON.
pub fn format_json(report: &VulnReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string())
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max - 3])
    } else {
        s.to_string()
    }
}
