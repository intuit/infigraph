use std::io::Write;
use std::path::Path;

use anyhow::Result;
use infigraph_core::diagnostics::{diagnose, CheckStatus, Health};

pub(crate) fn run(root: &Path, json: bool) -> Result<i32> {
    let report = diagnose(root);
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
    } else {
        writeln!(out, "Infigraph Doctor\n")?;
        for check in &report.checks {
            let label = match check.status {
                CheckStatus::Pass => "ok",
                CheckStatus::Unavailable => "info",
                CheckStatus::Unknown => match check.severity {
                    infigraph_core::diagnostics::Severity::Info => "info/unknown",
                    infigraph_core::diagnostics::Severity::Warning => "warning/unknown",
                    infigraph_core::diagnostics::Severity::Error => "error/unknown",
                },
                CheckStatus::Problem => match check.severity {
                    infigraph_core::diagnostics::Severity::Error => "error",
                    _ => "warning",
                },
            };
            let code = serde_json::to_value(check.code)?;
            writeln!(
                out,
                "[{label}] {}: {}",
                code.as_str().unwrap_or("UNKNOWN"),
                check.message
            )?;
            if let Some(action) = &check.recommended_action {
                writeln!(out, "  Next: {action}")?;
            }
        }
        let status = match report.status {
            Health::Healthy => "HEALTHY",
            Health::Degraded => "DEGRADED",
            Health::Unhealthy => "UNHEALTHY",
        };
        writeln!(out, "\nResult: {status}")?;
    }
    out.flush()?;
    Ok(report.exit_code())
}
