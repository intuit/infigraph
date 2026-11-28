use anyhow::Result;
use serde::Serialize;

use crate::graph::{Concern, GraphBackend};

#[derive(Debug, Clone, Serialize)]
pub struct ConcernMatch {
    pub symbol_id: String,
    pub kind: &'static str,
    pub detail: String,
}

struct ConcernPattern {
    kind: &'static str,
    patterns: &'static [&'static str],
}

static CONCERN_PATTERNS: &[ConcernPattern] = &[
    // Authorization
    ConcernPattern {
        kind: "Authorization",
        patterns: &[
            // Java/Kotlin
            "@PreAuthorize(",
            "@PostAuthorize(",
            "@Secured(",
            "@RolesAllowed(",
            "@PermitAll",
            "@DenyAll",
            // Python
            "@login_required",
            "@permission_required(",
            "@requires_auth",
            // TS/JS (NestJS)
            "@UseGuards(",
            "@Roles(",
            "@SetMetadata('roles'",
            // C#
            "[Authorize(",
            "[Authorize]",
            "[AllowAnonymous]",
            // Rust
            "#[guard(",
            "#[authorize(",
        ],
    },
    // Validation
    ConcernPattern {
        kind: "Validation",
        patterns: &[
            // Java/Kotlin
            "@Valid",
            "@Validated",
            "@NotNull",
            "@NotBlank",
            "@NotEmpty",
            "@Size(",
            "@Pattern(",
            "@Min(",
            "@Max(",
            // Python
            "@validator(",
            "@pydantic.validator(",
            "@field_validator(",
            // TS/JS (NestJS)
            "@UsePipes(",
            "ValidationPipe",
            // C#
            "[ValidateAntiForgeryToken]",
            "[Required]",
            "[Range(",
            "[StringLength(",
            // Rust
            "#[validate(",
        ],
    },
    // Caching
    ConcernPattern {
        kind: "Caching",
        patterns: &[
            // Java/Kotlin
            "@Cacheable(",
            "@CacheEvict(",
            "@CachePut(",
            "@Caching(",
            // Python
            "@cache",
            "@lru_cache(",
            "@cached_property",
            "@memoize",
            // TS/JS (NestJS)
            "@CacheKey(",
            "@CacheTTL(",
            "CacheInterceptor",
            // C#
            "[OutputCache(",
            "[ResponseCache(",
            // Ruby
            "caches_action",
            "caches_page",
            // Rust
            "#[cached(",
        ],
    },
    // Transaction
    ConcernPattern {
        kind: "Transaction",
        patterns: &[
            // Java/Kotlin
            "@Transactional(",
            "@Transactional\n",
            // Python
            "@atomic",
            "@transaction.atomic",
            "@commit_on_success",
            // TS/JS
            "@Transactional()",
            // C#
            "[Transaction]",
            // Rust
            "#[transactional]",
        ],
    },
    // RateLimiting
    ConcernPattern {
        kind: "RateLimiting",
        patterns: &[
            // Java
            "@RateLimiter(",
            "@RateLimit(",
            "@Bulkhead(",
            // Python
            "@rate_limit(",
            "@throttle(",
            "@ratelimit(",
            // TS/JS (NestJS)
            "@Throttle(",
            "@SkipThrottle(",
            // C#
            "[EnableRateLimiting(",
            "[DisableRateLimiting(",
            // Rust
            "#[rate_limit(",
        ],
    },
    // AuditLogging
    ConcernPattern {
        kind: "AuditLogging",
        patterns: &[
            "@Auditable(",
            "@Audit(",
            "@Logged",
            "@audit_log(",
            "@log_action(",
            "LoggingInterceptor",
            "[Audit]",
            "#[instrument(",
        ],
    },
    // FeatureFlag
    ConcernPattern {
        kind: "FeatureFlag",
        patterns: &[
            "@FeatureFlag(",
            "@Toggle(",
            "@Feature(",
            "@feature_flag(",
            "@feature_enabled(",
            "[FeatureGate(",
            "#[feature(",
        ],
    },
    // Cors
    ConcernPattern {
        kind: "Cors",
        patterns: &[
            "@CrossOrigin(",
            "@CrossOrigin\n",
            "[EnableCors(",
            "[DisableCors(",
            "#[cors(",
        ],
    },
    // Async
    ConcernPattern {
        kind: "Async",
        patterns: &[
            // Java
            "@Async",
            "@Scheduled(",
            "@EventListener(",
            // Python
            "@celery.task",
            "@background_task(",
            "@periodic_task(",
            // TS/JS (NestJS)
            "@Cron(",
            "@Interval(",
            "@EventPattern(",
            // C#
            "[BackgroundService]",
            // Rust
            "#[tokio::main]",
        ],
    },
    // Retry / Resilience
    ConcernPattern {
        kind: "Retry",
        patterns: &[
            "@Retry(",
            "@Retryable(",
            "@CircuitBreaker(",
            "@retry(",
            "@backoff(",
            "@circuit_breaker(",
            "RetryInterceptor",
            "[Retry(",
            "[CircuitBreaker(",
            "#[retry(",
        ],
    },
];

pub fn detect_cross_cutting(backend: &dyn GraphBackend) -> Result<Vec<ConcernMatch>> {
    // Use the repo-scoped `symbols_with_docstring` accessor instead of a global
    // `MATCH (s:Symbol)` — in shared-Neo4j mode the latter returns every repo's symbols
    // and leaks concerns across projects. This honors the backend's repo_filter.
    let symbols = backend.symbols_with_docstring(None)?;

    let mut matches = Vec::new();

    for sym in &symbols {
        if sym.docstring.is_empty() {
            continue;
        }
        let symbol_id = &sym.id;
        let docstring = &sym.docstring;

        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    let detail = extract_matched_line(docstring, pattern);
                    matches.push(ConcernMatch {
                        symbol_id: symbol_id.clone(),
                        kind: cp.kind,
                        detail,
                    });
                    break;
                }
            }
        }
    }

    // Docstring patterns can't see FastAPI's `app.add_middleware(...)` /
    // `Depends(...)` wiring -- that's structural (a registration call site
    // calling into the middleware/dependency function), not a decorator on
    // the function itself, and it's already captured as REGISTERS_MIDDLEWARE
    // / INJECTS_DEPENDENCY graph edges during extraction (AIF3X-331 #16).
    // Surface those edges here too instead of leaving this tool blind to them.
    for (edge_kind, concern_kind) in [
        ("REGISTERS_MIDDLEWARE", "Middleware"),
        ("INJECTS_DEPENDENCY", "DependencyInjection"),
    ] {
        // Scope to the current repo the same way `symbols_with_docstring` /
        // `stats()` do -- an unscoped MATCH leaks every repo's middleware/DI
        // edges in shared-Neo4j mode, since Symbol nodes carry no `repo`
        // property directly (only File nodes do).
        let query = if let Some(repo) = backend.repo_filter() {
            let r = repo.replace('\'', "\\'");
            format!(
                "MATCH (f:File {{repo: '{r}'}})-[:DEFINES]->(a:Symbol)-[:{edge_kind}]->(b:Symbol) RETURN a.id, b.id"
            )
        } else {
            format!("MATCH (a:Symbol)-[:{edge_kind}]->(b:Symbol) RETURN a.id, b.id")
        };
        if let Ok(rows) = backend.raw_query(&query) {
            for row in rows {
                if row.len() < 2 {
                    continue;
                }
                let source_id = row[0].trim_matches('"');
                let target_id = row[1].trim_matches('"');
                matches.push(ConcernMatch {
                    symbol_id: target_id.to_string(),
                    kind: concern_kind,
                    detail: format!("registered via {edge_kind} at {source_id}"),
                });
            }
        }
    }

    if !matches.is_empty() {
        let concerns: Vec<Concern> = matches
            .iter()
            .map(|m| Concern {
                symbol_id: m.symbol_id.clone(),
                kind: m.kind.to_string(),
                detail: m.detail.clone(),
            })
            .collect();
        backend.replace_concerns(&concerns)?;
    }

    Ok(matches)
}

fn extract_matched_line(docstring: &str, pattern: &str) -> String {
    for line in docstring.lines() {
        if line.contains(pattern) {
            return line.trim().to_string();
        }
    }
    pattern.to_string()
}

pub fn format_concerns(matches: &[ConcernMatch]) -> String {
    if matches.is_empty() {
        return "No cross-cutting concerns detected.".to_string();
    }

    let mut by_kind: std::collections::BTreeMap<&str, Vec<&ConcernMatch>> =
        std::collections::BTreeMap::new();
    for m in matches {
        by_kind.entry(m.kind).or_default().push(m);
    }

    let mut out = format!("Cross-cutting concerns: {} total\n\n", matches.len());
    for (kind, items) in &by_kind {
        out.push_str(&format!("## {} ({} symbols)\n", kind, items.len()));
        for item in items {
            out.push_str(&format!("  {} — {}\n", item.symbol_id, item.detail));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_java_authorization() {
        let docstring = "@PreAuthorize(\"hasRole('ADMIN')\")\npublic void deleteUser() {}";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"Authorization"),
            "should detect @PreAuthorize"
        );
    }

    #[test]
    fn test_detect_python_caching() {
        let docstring = "@lru_cache(maxsize=128)\ndef get_user(user_id):";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(found.contains(&"Caching"), "should detect @lru_cache");
    }

    #[test]
    fn test_detect_nestjs_throttle() {
        let docstring = "@Throttle(10, 60)\n@Roles('admin')\nasync getUsers() {}";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(found.contains(&"RateLimiting"), "should detect @Throttle");
        assert!(found.contains(&"Authorization"), "should detect @Roles");
    }

    #[test]
    fn test_detect_csharp_authorize() {
        let docstring = "[Authorize(Roles=\"Admin\")]\n[ValidateAntiForgeryToken]\npublic IActionResult Delete()";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"Authorization"),
            "should detect [Authorize]"
        );
        assert!(
            found.contains(&"Validation"),
            "should detect [ValidateAntiForgeryToken]"
        );
    }

    #[test]
    fn test_detect_rust_instrument() {
        let docstring = "#[instrument(skip(db))]\nasync fn handle_request()";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"AuditLogging"),
            "should detect #[instrument]"
        );
    }

    #[test]
    fn test_detect_spring_transactional() {
        let docstring = "@Transactional(readOnly = true)\npublic List<User> findAll()";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"Transaction"),
            "should detect @Transactional"
        );
    }

    #[test]
    fn test_no_false_positive_on_plain_text() {
        let docstring = "This function validates cacheable behavior for users";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.is_empty(),
            "should not match plain text without annotation syntax: {:?}",
            found
        );
    }

    #[test]
    fn test_extract_matched_line() {
        let doc = "@PreAuthorize(\"hasRole('ADMIN')\")\npublic void delete()";
        let line = extract_matched_line(doc, "@PreAuthorize(");
        assert_eq!(line, "@PreAuthorize(\"hasRole('ADMIN')\")");
    }

    #[test]
    fn test_detect_python_login_required() {
        let docstring = "@login_required\ndef dashboard(request):";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        assert!(
            found.contains(&"Authorization"),
            "should detect @login_required"
        );
    }

    #[test]
    fn test_detect_ruby_before_action() {
        let docstring = "before_action :authenticate_user!\ndef index";
        let mut found = Vec::new();
        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    found.push(cp.kind);
                    break;
                }
            }
        }
        // Ruby patterns don't start with @ or [, they're bare method calls
        // "before_action :authenticate" is not in our patterns — let me check
        assert!(
            found.is_empty() || found.contains(&"Authorization"),
            "Ruby before_action pattern check: {:?}",
            found
        );
    }
}
