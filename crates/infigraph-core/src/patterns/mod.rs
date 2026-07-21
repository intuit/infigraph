//! Design pattern detection via graph queries.
//!
//! Detects common design patterns (Factory, Observer, Singleton, Strategy,
//! Decorator) by querying the existing call/inheritance graph.

use anyhow::Result;
use serde::Serialize;

use crate::graph::GraphBackend;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single detected design-pattern instance.
#[derive(Debug, Clone, Serialize)]
pub struct PatternMatch {
    /// Pattern name: "Factory", "Observer", "Singleton", "Strategy", "Decorator"
    pub pattern: String,
    /// Confidence level: "high", "medium", "low"
    pub confidence: String,
    /// Symbols participating in the pattern and their roles.
    pub participants: Vec<PatternParticipant>,
    /// Primary file where the pattern is anchored.
    pub file: String,
}

/// A symbol participating in a detected pattern.
#[derive(Debug, Clone, Serialize)]
pub struct PatternParticipant {
    /// Role within the pattern (e.g. "Creator", "Product", "Subject").
    pub role: String,
    /// Fully-qualified symbol name.
    pub symbol: String,
    /// File where the symbol lives.
    pub file: String,
}

/// Aggregated pattern-detection report.
#[derive(Debug, Clone, Serialize)]
pub struct PatternReport {
    pub patterns: Vec<PatternMatch>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run all pattern detectors and return a combined report.
pub fn detect_all(backend: &dyn GraphBackend) -> Result<PatternReport> {
    let mut patterns = Vec::new();
    patterns.extend(detect_factory(backend));
    patterns.extend(detect_singleton(backend));
    patterns.extend(detect_observer(backend));
    patterns.extend(detect_strategy(backend));
    patterns.extend(detect_decorator(backend));

    Ok(PatternReport { patterns })
}

/// Run detectors and optionally keep only the given pattern type.
pub fn detect_filtered(backend: &dyn GraphBackend, filter: Option<&str>) -> Result<PatternReport> {
    let mut report = detect_all(backend)?;
    if let Some(name) = filter {
        let lower = name.to_lowercase();
        report
            .patterns
            .retain(|p| p.pattern.to_lowercase() == lower);
    }
    Ok(report)
}

/// Render report as human-readable text grouped by pattern type.
pub fn format_report(report: &PatternReport) -> String {
    if report.patterns.is_empty() {
        return "No design patterns detected.\n".to_string();
    }

    let mut out = String::new();
    let groups = group_by_pattern(&report.patterns);

    for (pattern, matches) in &groups {
        out.push_str(&format!(
            "\n=== {} Pattern ({} instance{}) ===\n",
            pattern,
            matches.len(),
            if matches.len() == 1 { "" } else { "s" }
        ));
        for (i, m) in matches.iter().enumerate() {
            out.push_str(&format!("\n  {}. [{}] {}\n", i + 1, m.confidence, m.file));
            for p in &m.participants {
                out.push_str(&format!("     {:<14} {} ({})\n", p.role, p.symbol, p.file));
            }
        }
    }

    let total: usize = groups.iter().map(|(_, v)| v.len()).sum();
    out.push_str(&format!(
        "\nTotal: {} pattern instance(s) detected.\n",
        total
    ));
    out
}

/// Render report as pretty-printed JSON.
pub fn format_json(report: &PatternReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn group_by_pattern(matches: &[PatternMatch]) -> Vec<(String, Vec<&PatternMatch>)> {
    let order = ["Factory", "Singleton", "Observer", "Strategy", "Decorator"];
    let mut groups: Vec<(String, Vec<&PatternMatch>)> = Vec::new();
    for name in &order {
        let items: Vec<&PatternMatch> = matches.iter().filter(|m| m.pattern == *name).collect();
        if !items.is_empty() {
            groups.push((name.to_string(), items));
        }
    }
    // Catch any pattern names not in the standard order
    for m in matches {
        if !order.contains(&m.pattern.as_str()) {
            if let Some(g) = groups.iter_mut().find(|(n, _)| *n == m.pattern) {
                g.1.push(m);
            } else {
                groups.push((m.pattern.clone(), vec![m]));
            }
        }
    }
    groups
}

fn strip_quotes(s: &str) -> String {
    s.trim_matches('"').trim_matches('\'').to_string()
}

// ---------------------------------------------------------------------------
// Factory Pattern
// ---------------------------------------------------------------------------
// A class/struct with methods that create or return instances of subtypes.
// High confidence when the method name contains create/build/make/factory.

fn detect_factory(backend: &dyn GraphBackend) -> Vec<PatternMatch> {
    // Find methods that call constructors/classes that have INHERITS edges
    let query = "\
        MATCH (creator:Symbol)-[:CALLS]->(product:Symbol) \
        WHERE creator.kind = 'Method' \
        AND product.kind IN ['Class', 'Function'] \
        AND EXISTS { MATCH (product)-[:INHERITS]->(:Symbol) } \
        RETURN DISTINCT creator.parent, creator.name, creator.file, product.name, product.file";

    let rows = match backend.raw_query(query) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut results: Vec<PatternMatch> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for row in &rows {
        if row.len() < 5 {
            continue;
        }
        let creator_parent = strip_quotes(&row[0]);
        let creator_name = strip_quotes(&row[1]);
        let creator_file = strip_quotes(&row[2]);
        let product_name = strip_quotes(&row[3]);
        let product_file = strip_quotes(&row[4]);

        let key = format!("{}::{}", creator_parent, creator_name);
        if !seen.insert(key) {
            continue;
        }

        let name_lower = creator_name.to_lowercase();
        let confidence = if name_lower.contains("create")
            || name_lower.contains("build")
            || name_lower.contains("make")
            || name_lower.contains("factory")
            || name_lower.contains("new_")
        {
            "high"
        } else {
            "medium"
        };

        results.push(PatternMatch {
            pattern: "Factory".to_string(),
            confidence: confidence.to_string(),
            participants: vec![
                PatternParticipant {
                    role: "Creator".to_string(),
                    symbol: format!("{}::{}", creator_parent, creator_name),
                    file: creator_file.clone(),
                },
                PatternParticipant {
                    role: "Product".to_string(),
                    symbol: product_name,
                    file: product_file,
                },
            ],
            file: creator_file,
        });
    }
    results
}

// ---------------------------------------------------------------------------
// Singleton Pattern
// ---------------------------------------------------------------------------
// Classes with a static instance-access method (getInstance, instance, shared,
// get_instance, etc.).

fn detect_singleton(backend: &dyn GraphBackend) -> Vec<PatternMatch> {
    let singleton_names = [
        "getInstance",
        "instance",
        "shared",
        "get_instance",
        "getDefault",
        "sharedInstance",
    ];

    let mut results: Vec<PatternMatch> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for accessor in &singleton_names {
        let query = format!(
            "MATCH (cls:Symbol), (method:Symbol) \
             WHERE cls.kind = 'Class' \
             AND method.kind = 'Method' \
             AND method.parent = cls.name \
             AND method.name = '{}' \
             RETURN DISTINCT cls.name, cls.file, method.name",
            accessor
        );

        let rows = match backend.raw_query(&query) {
            Ok(r) => r,
            Err(_) => continue,
        };

        for row in &rows {
            if row.len() < 3 {
                continue;
            }
            let cls_name = strip_quotes(&row[0]);
            let cls_file = strip_quotes(&row[1]);
            let method_name = strip_quotes(&row[2]);

            if !seen.insert(cls_name.clone()) {
                continue;
            }

            results.push(PatternMatch {
                pattern: "Singleton".to_string(),
                confidence: "high".to_string(),
                participants: vec![
                    PatternParticipant {
                        role: "Singleton".to_string(),
                        symbol: cls_name.clone(),
                        file: cls_file.clone(),
                    },
                    PatternParticipant {
                        role: "Accessor".to_string(),
                        symbol: format!("{}::{}", cls_name, method_name),
                        file: cls_file.clone(),
                    },
                ],
                file: cls_file,
            });
        }
    }
    results
}

// ---------------------------------------------------------------------------
// Observer Pattern
// ---------------------------------------------------------------------------
// Subject with subscribe/register + notify/emit methods.

fn detect_observer(backend: &dyn GraphBackend) -> Vec<PatternMatch> {
    // Step 1: find methods whose names suggest registration of listeners
    let register_query = "\
        MATCH (reg:Symbol) \
        WHERE reg.kind = 'Method' \
        AND (reg.name CONTAINS 'register' \
             OR reg.name CONTAINS 'subscribe' \
             OR reg.name CONTAINS 'add_listener' \
             OR reg.name CONTAINS 'addEventListener' \
             OR reg.name CONTAINS 'addObserver' \
             OR reg.name CONTAINS 'on_') \
        RETURN DISTINCT reg.parent, reg.name, reg.file";

    let reg_rows = match backend.raw_query(register_query) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    if reg_rows.is_empty() {
        return Vec::new();
    }

    // Build set of parent classes that have register methods
    let mut register_parents = std::collections::HashMap::<String, (String, String)>::new();
    for row in &reg_rows {
        if row.len() < 3 {
            continue;
        }
        let parent = strip_quotes(&row[0]);
        let method = strip_quotes(&row[1]);
        let file = strip_quotes(&row[2]);
        if !parent.is_empty() {
            register_parents.entry(parent).or_insert((file, method));
        }
    }

    // Step 2: check which of those classes also have notify-like methods
    let notify_query = "\
        MATCH (n:Symbol) \
        WHERE n.kind = 'Method' \
        AND (n.name CONTAINS 'notify' \
             OR n.name CONTAINS 'emit' \
             OR n.name CONTAINS 'publish' \
             OR n.name CONTAINS 'dispatch' \
             OR n.name CONTAINS 'fire') \
        RETURN DISTINCT n.parent, n.name, n.file";

    let notify_rows = match backend.raw_query(notify_query) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut results: Vec<PatternMatch> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for row in &notify_rows {
        if row.len() < 3 {
            continue;
        }
        let parent = strip_quotes(&row[0]);
        let notify_name = strip_quotes(&row[1]);
        let file = strip_quotes(&row[2]);

        if let Some((reg_file, reg_method)) = register_parents.get(&parent) {
            if !seen.insert(parent.clone()) {
                continue;
            }
            results.push(PatternMatch {
                pattern: "Observer".to_string(),
                confidence: "high".to_string(),
                participants: vec![
                    PatternParticipant {
                        role: "Subject".to_string(),
                        symbol: parent.clone(),
                        file: reg_file.clone(),
                    },
                    PatternParticipant {
                        role: "Register".to_string(),
                        symbol: format!("{}::{}", parent, reg_method),
                        file: reg_file.clone(),
                    },
                    PatternParticipant {
                        role: "Notify".to_string(),
                        symbol: format!("{}::{}", parent, notify_name),
                        file,
                    },
                ],
                file: reg_file.clone(),
            });
        }
    }
    results
}

// ---------------------------------------------------------------------------
// Strategy Pattern
// ---------------------------------------------------------------------------
// An interface/trait with 3+ classes inheriting from it.

fn detect_strategy(backend: &dyn GraphBackend) -> Vec<PatternMatch> {
    // Kuzu/lbug may not support WITH + aggregation well, so fetch all
    // INHERITS edges and aggregate in Rust.
    let query = "\
        MATCH (impl:Symbol)-[:INHERITS]->(iface:Symbol) \
        WHERE iface.kind IN ['Class', 'Interface', 'Trait'] \
        RETURN iface.name, iface.file, impl.name, impl.file";

    let rows = match backend.raw_query(query) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    // Group implementations by interface
    let mut iface_impls: std::collections::HashMap<String, (String, Vec<(String, String)>)> =
        std::collections::HashMap::new();

    for row in &rows {
        if row.len() < 4 {
            continue;
        }
        let iface_name = strip_quotes(&row[0]);
        let iface_file = strip_quotes(&row[1]);
        let impl_name = strip_quotes(&row[2]);
        let impl_file = strip_quotes(&row[3]);

        let entry = iface_impls
            .entry(iface_name)
            .or_insert_with(|| (iface_file, Vec::new()));
        entry.1.push((impl_name, impl_file));
    }

    let mut results: Vec<PatternMatch> = Vec::new();

    for (iface_name, (iface_file, impls)) in &iface_impls {
        if impls.len() < 3 {
            continue;
        }

        let confidence = if impls.len() >= 5 { "high" } else { "medium" };

        let mut participants = vec![PatternParticipant {
            role: "Strategy".to_string(),
            symbol: iface_name.clone(),
            file: iface_file.clone(),
        }];

        for (impl_name, impl_file) in impls {
            participants.push(PatternParticipant {
                role: "ConcreteStrategy".to_string(),
                symbol: impl_name.clone(),
                file: impl_file.clone(),
            });
        }

        results.push(PatternMatch {
            pattern: "Strategy".to_string(),
            confidence: confidence.to_string(),
            participants,
            file: iface_file.clone(),
        });
    }

    results
}

// ---------------------------------------------------------------------------
// Decorator / Wrapper Pattern
// ---------------------------------------------------------------------------
// A class that inherits from X AND calls methods on the base type.

fn detect_decorator(backend: &dyn GraphBackend) -> Vec<PatternMatch> {
    let query = "\
        MATCH (decorator:Symbol)-[:INHERITS]->(base:Symbol) \
        WHERE decorator.kind = 'Class' \
        AND base.kind IN ['Class', 'Interface', 'Trait'] \
        AND EXISTS { \
            MATCH (decorator)-[:CALLS]->(base_method:Symbol) \
            WHERE base_method.parent = base.name \
        } \
        RETURN DISTINCT decorator.name, decorator.file, base.name, base.file";

    let rows = match backend.raw_query(query) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut results: Vec<PatternMatch> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for row in &rows {
        if row.len() < 4 {
            continue;
        }
        let dec_name = strip_quotes(&row[0]);
        let dec_file = strip_quotes(&row[1]);
        let base_name = strip_quotes(&row[2]);
        let base_file = strip_quotes(&row[3]);

        if !seen.insert(format!("{}>{}", dec_name, base_name)) {
            continue;
        }

        // Higher confidence if the decorator name suggests wrapping
        let name_lower = dec_name.to_lowercase();
        let confidence = if name_lower.contains("decorator")
            || name_lower.contains("wrapper")
            || name_lower.contains("proxy")
            || name_lower.contains("adapter")
        {
            "high"
        } else {
            "medium"
        };

        results.push(PatternMatch {
            pattern: "Decorator".to_string(),
            confidence: confidence.to_string(),
            participants: vec![
                PatternParticipant {
                    role: "Decorator".to_string(),
                    symbol: dec_name,
                    file: dec_file.clone(),
                },
                PatternParticipant {
                    role: "Component".to_string(),
                    symbol: base_name,
                    file: base_file,
                },
            ],
            file: dec_file,
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_formats() {
        let report = PatternReport { patterns: vec![] };
        assert_eq!(format_report(&report), "No design patterns detected.\n");
    }

    #[test]
    fn json_roundtrip() {
        let report = PatternReport {
            patterns: vec![PatternMatch {
                pattern: "Factory".to_string(),
                confidence: "high".to_string(),
                participants: vec![PatternParticipant {
                    role: "Creator".to_string(),
                    symbol: "MyFactory::create".to_string(),
                    file: "src/factory.rs".to_string(),
                }],
                file: "src/factory.rs".to_string(),
            }],
        };
        let json = format_json(&report);
        assert!(json.contains("Factory"));
        assert!(json.contains("high"));
    }

    #[test]
    fn strip_quotes_works() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
        assert_eq!(strip_quotes("plain"), "plain");
    }

    #[test]
    fn report_groups_by_pattern() {
        let report = PatternReport {
            patterns: vec![
                PatternMatch {
                    pattern: "Singleton".to_string(),
                    confidence: "high".to_string(),
                    participants: vec![],
                    file: "a.py".to_string(),
                },
                PatternMatch {
                    pattern: "Factory".to_string(),
                    confidence: "medium".to_string(),
                    participants: vec![],
                    file: "b.py".to_string(),
                },
                PatternMatch {
                    pattern: "Singleton".to_string(),
                    confidence: "high".to_string(),
                    participants: vec![],
                    file: "c.py".to_string(),
                },
            ],
        };
        let text = format_report(&report);
        // Factory should come before Singleton in the output (canonical order)
        let factory_pos = text.find("Factory Pattern").unwrap();
        let singleton_pos = text.find("Singleton Pattern").unwrap();
        assert!(factory_pos < singleton_pos);
        assert!(text.contains("Total: 3 pattern instance(s)"));
    }
}
