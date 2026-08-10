mod detect;
mod format;
mod rules;

pub use detect::*;
pub use format::*;
pub use rules::*;

#[cfg(test)]
mod tests {
    use super::detect::scan_file;
    use super::*;
    use std::io::Write;

    fn scan_str(content: &str, ext: &str) -> Vec<Finding> {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(format!("test.{}", ext));
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        let mut stats = ScanStats::default();
        scan_file(&file, &format!("test.{}", ext), ext, &mut stats).unwrap();
        stats.findings
    }

    #[test]
    fn detects_pickle_loads() {
        let findings = scan_str("data = pickle.loads(user_input)", "py");
        assert!(findings.iter().any(|f| f.rule_id == "SEC030"));
    }

    #[test]
    fn detects_hardcoded_password() {
        let findings = scan_str("password = \"s3cr3t\"", "py");
        assert!(findings.iter().any(|f| f.rule_id == "SEC010"));
    }

    #[test]
    fn detects_eval_js() {
        let findings = scan_str("eval(userInput)", "js");
        assert!(findings.iter().any(|f| f.rule_id == "SEC021"));
    }

    #[test]
    fn detects_md5() {
        let findings = scan_str("digest = md5(password)", "py");
        assert!(findings.iter().any(|f| f.category == Category::WeakCrypto));
    }

    #[test]
    fn detects_innerhtml() {
        let findings = scan_str("el.innerHTML = userInput", "js");
        assert!(findings.iter().any(|f| f.rule_id == "SEC100"));
    }

    #[test]
    fn no_false_positive_yaml_safe() {
        let findings = scan_str("data = yaml.load(f, loader=yaml.SafeLoader)", "py");
        assert!(!findings.iter().any(|f| f.rule_id == "SEC032"));
    }

    #[test]
    fn sanitizer_suppresses_sql_injection() {
        // AIF3X-331 #17: execute() now requires same-line SQL evidence (to
        // stop flagging bare async HTTP client calls), so the fixture must
        // carry that evidence on the execute() line itself for SEC001 to fire
        // at all -- a bare `cursor.execute(query)` is now a known false
        // negative (cross-line SQL construction is taint's job, not this
        // line-heuristic's).
        let code = "safe = sanitize_sql(user_input)\ncursor.execute(\"SELECT * FROM users WHERE id = \" + safe)";
        let findings = scan_str(code, "py");
        let sql_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.category == Category::SqlInjection)
            .collect();
        assert!(!sql_findings.is_empty(), "should still detect execute()");
        assert!(
            sql_findings.iter().all(|f| f.suppressed),
            "should be suppressed by sanitize_sql"
        );
        assert!(sql_findings[0].sanitizer_hint.as_deref() == Some("sanitize_sql"));
    }

    #[test]
    fn sanitizer_suppresses_xss_dompurify() {
        let code = "const clean = DOMPurify.sanitize(content);\nel.innerHTML = clean;";
        let findings = scan_str(code, "js");
        let xss: Vec<_> = findings
            .iter()
            .filter(|f| f.category == Category::XssRisk)
            .collect();
        assert!(!xss.is_empty());
        assert!(
            xss.iter().all(|f| f.suppressed),
            "innerHTML near DOMPurify should be suppressed"
        );
    }

    #[test]
    fn no_suppression_without_sanitizer() {
        let code = "cursor.execute(\"SELECT * FROM users WHERE name = \" + user_input)";
        let findings = scan_str(code, "py");
        let sql: Vec<_> = findings
            .iter()
            .filter(|f| f.category == Category::SqlInjection)
            .collect();
        assert!(!sql.is_empty());
        assert!(
            sql.iter().all(|f| !f.suppressed),
            "no sanitizer = not suppressed"
        );
    }

    #[test]
    fn sanitizer_suppresses_command_injection() {
        let code = "safe_arg = shlex.quote(user_input)\nos.system(safe_arg)";
        let findings = scan_str(code, "py");
        let cmd: Vec<_> = findings
            .iter()
            .filter(|f| f.category == Category::CommandInjection)
            .collect();
        assert!(!cmd.is_empty());
        assert!(
            cmd.iter().all(|f| f.suppressed),
            "shlex.quote nearby should suppress"
        );
    }

    #[test]
    fn no_false_positive_fstring_select_prefix() {
        // SEC004/SEC005 patterns ("f\"select", "f'select") used to be a bare
        // substring match, so any f-string starting with a word beginning
        // "select" (selected, selection, ...) false-positived as SQL
        // injection -- e.g. logger.debug(f"selected_model_region={x}").
        let code = "logger.debug(f\"selected_model_region={selected_model_region}\")";
        let findings = scan_str(code, "py");
        assert!(
            !findings
                .iter()
                .any(|f| f.category == Category::SqlInjection),
            "f-string starting with 'selected_' is not a SQL SELECT"
        );
    }

    #[test]
    fn detects_fstring_sql_select() {
        let findings = scan_str("query = f\"select * from users where id={uid}\"", "py");
        assert!(findings.iter().any(|f| f.rule_id == "SEC004"));
    }

    #[test]
    fn sanitizer_suppresses_path_traversal() {
        let code = "safe = os.path.realpath(user_path)\nopen(safe)";
        let findings = scan_str(code, "py");
        let path_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.category == Category::PathTraversal)
            .collect();
        for f in &path_findings {
            assert!(
                f.suppressed,
                "realpath nearby should suppress path traversal"
            );
        }
    }
}
