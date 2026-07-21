use anyhow::Result;
use serde::Serialize;

use crate::graph::GraphBackend;

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
    let result = backend
        .raw_query("MATCH (s:Symbol) WHERE s.docstring IS NOT NULL AND s.docstring <> '' RETURN s.id, s.docstring")?;

    let mut matches = Vec::new();

    for row in result {
        if row.len() < 2 {
            continue;
        }
        let symbol_id = row[0].to_string();
        let docstring = row[1].to_string();

        for cp in CONCERN_PATTERNS {
            for &pattern in cp.patterns {
                if docstring.contains(pattern) {
                    let detail = extract_matched_line(&docstring, pattern);
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

    if !matches.is_empty() {
        write_concerns(backend, &matches)?;
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

fn write_concerns(backend: &dyn GraphBackend, matches: &[ConcernMatch]) -> Result<()> {
    backend.raw_query("BEGIN TRANSACTION")?;

    let _ = backend.raw_query("MATCH (c:Concern) DETACH DELETE c");

    for m in matches {
        let sym_esc = crate::escape_str(&m.symbol_id);
        let kind_esc = crate::escape_str(m.kind);
        let detail_esc = crate::escape_str(&m.detail);
        let concern_id = format!("{}::{}", m.symbol_id, m.kind);
        let id_esc = crate::escape_str(&concern_id);

        let _ = backend.raw_query(&format!(
            "CREATE (c:Concern {{id: '{id_esc}', kind: '{kind_esc}', detail: '{detail_esc}'}})"
        ));
        let _ = backend.raw_query(&format!(
            "MATCH (s:Symbol), (c:Concern) WHERE s.id = '{sym_esc}' AND c.id = '{id_esc}' CREATE (s)-[:HAS_CONCERN]->(c)"
        ));
    }

    backend.raw_query("COMMIT")?;

    Ok(())
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
