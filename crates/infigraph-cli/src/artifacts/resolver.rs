use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResolverInput {
    pub mcp_path: String,
    pub os: String,
    pub home: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ResolverData {
    pub path: String,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum ResolverOutput {
    Ok { data: ResolverData },
    Skip { message: String },
    Error { message: String },
}

/// Runs a resolver executable (`resolver_cmd[0]` resolved relative to `cwd`,
/// with `resolver_cmd[1..]` as its arguments), feeding it the standard
/// resolver-contract JSON on stdin and parsing its JSON stdout response.
pub(crate) fn run_resolver(
    resolver_cmd: &[String],
    cwd: &Path,
    mcp_path: &str,
    home: &Path,
) -> Result<ResolverOutput> {
    anyhow::ensure!(
        !resolver_cmd.is_empty(),
        "resolver command must not be empty"
    );

    let input = ResolverInput {
        mcp_path: mcp_path.to_string(),
        os: std::env::consts::OS.to_string(),
        home: home.to_string_lossy().to_string(),
    };
    let input_json = serde_json::to_string(&input)?;

    let mut command = std::process::Command::new(&resolver_cmd[0]);
    command
        .args(&resolver_cmd[1..])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn resolver {}", resolver_cmd[0]))?;

    {
        use std::io::Write as _;
        let stdin = child.stdin.as_mut().expect("stdin was piped");
        stdin
            .write_all(input_json.as_bytes())
            .context("failed to write resolver input")?;
    }

    let output = child
        .wait_with_output()
        .context("failed to wait for resolver process")?;

    anyhow::ensure!(
        output.status.success(),
        "resolver {} exited with {:?}: {}",
        resolver_cmd[0],
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).with_context(|| {
        format!(
            "resolver {} returned invalid JSON: {stdout}",
            resolver_cmd[0]
        )
    })
}

/// Builds the actual command to invoke a resolver script, interpreter-aware
/// by extension rather than relying on shebang-based direct execution (which
/// only works on Unix). `.py` scripts are launched via an explicit Python
/// interpreter on every platform; anything else falls back to direct
/// execution, which remains correct on Unix (and is a clear, immediate
/// spawn failure on Windows rather than a silent wrong-path resolver run --
/// this design bundles only `.py` resolvers, so that fallback path isn't
/// expected to be exercised today).
fn resolver_invocation(script_path: &Path, script_filename: &str) -> Vec<String> {
    if Path::new(script_filename)
        .extension()
        .and_then(|e| e.to_str())
        == Some("py")
    {
        let python = if cfg!(windows) { "python" } else { "python3" };
        vec![
            python.to_string(),
            script_path.to_string_lossy().to_string(),
        ]
    } else {
        vec![format!("./{script_filename}")]
    }
}

/// Writes a bundled/user-override resolver script's bytes to a temp file,
/// makes it executable (Unix only -- irrelevant for an explicitly
/// interpreter-invoked script, but harmless), spawns it via `run_resolver`,
/// and cleans up afterward. Needed because bundled content is embedded bytes
/// in the binary, not a real file on disk -- `run_resolver` alone can only
/// spawn a command that already exists at some real `cwd`.
pub(crate) fn run_resolver_from_script(
    script_bytes: &[u8],
    script_filename: &str,
    extra_args: &[String],
    mcp_path: &str,
    home: &Path,
) -> Result<ResolverOutput> {
    let tmp = tempfile::tempdir().context("failed to create temp directory for resolver script")?;
    let script_path = tmp.path().join(script_filename);
    std::fs::write(&script_path, script_bytes).with_context(|| {
        format!(
            "failed to write resolver script to {}",
            script_path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to make {} executable", script_path.display()))?;
    }

    let mut resolver_cmd = resolver_invocation(&script_path, script_filename);
    resolver_cmd.extend(extra_args.iter().cloned());

    run_resolver(&resolver_cmd, tmp.path(), mcp_path, home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    fn write_fake_resolver(dir: &std::path::Path, name: &str, script: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn resolver_ok_response_with_content() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"ok\",\"data\":{\"path\":\"/resolved/path.json\",\"content\":{\"command\":\"/bin/infigraph-mcp\"}}}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => {
                assert_eq!(data.path, "/resolved/path.json");
                assert_eq!(data.content.unwrap()["command"], "/bin/infigraph-mcp");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn resolver_ok_response_without_content() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"ok\",\"data\":{\"path\":\"/resolved/path.json\"}}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => {
                assert_eq!(data.path, "/resolved/path.json");
                assert!(data.content.is_none());
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn resolver_skip_response() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"skip\",\"message\":\"not installed\"}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Skip { message } => assert_eq!(message, "not installed"),
            other => panic!("expected Skip, got {other:?}"),
        }
    }

    #[test]
    fn resolver_error_response() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "resolve.sh",
            "#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"error\",\"message\":\"could not detect profile\"}\nEOF\n",
        );

        let output = run_resolver(
            &["./resolve.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Error { message } => assert_eq!(message, "could not detect profile"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn resolver_receives_correct_stdin_shape() {
        let dir = tempfile::tempdir().unwrap();
        write_fake_resolver(
            dir.path(),
            "echo_input.sh",
            // The received stdin is itself JSON (with embedded quotes), so it
            // must be escaped before being spliced into another JSON string
            // value -- otherwise the "echoed" response isn't valid JSON.
            "#!/usr/bin/env bash\ninput=$(cat)\nescaped=$(printf '%s' \"$input\" | sed 's/\"/\\\\\"/g')\necho \"{\\\"status\\\":\\\"ok\\\",\\\"data\\\":{\\\"path\\\":\\\"$escaped\\\"}}\"\n",
        );

        let output = run_resolver(
            &["./echo_input.sh".to_string()],
            dir.path(),
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => {
                assert!(data.path.contains("\"mcp_path\":\"/bin/infigraph-mcp\""));
                assert!(data.path.contains("\"home\":\"/home/x\""));
                assert!(data.path.contains("\"os\":"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn run_resolver_from_script_materializes_bundled_bytes_and_executes() {
        let script = b"#!/usr/bin/env bash\ncat <<'EOF'\n{\"status\":\"ok\",\"data\":{\"path\":\"/resolved/settings.json\"}}\nEOF\n";

        let output = run_resolver_from_script(
            script,
            "resolve-zed-path.sh",
            &[],
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => assert_eq!(data.path, "/resolved/settings.json"),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn run_resolver_from_script_invokes_python_scripts_via_explicit_interpreter() {
        // Regression test for the Windows resolver bug: a .py resolver must
        // never be spawned via shebang-based direct execution (Windows has
        // no such mechanism) -- it must always go through an explicit
        // python/python3 interpreter invocation, proven here by using a
        // script with NO shebang line at all (so this test would fail on
        // every platform, not just Windows, if the fix ever regressed back
        // to direct execution).
        let script = b"import json, sys\nprint(json.dumps({'status': 'ok', 'data': {'path': '/resolved/from-python.json'}}))\n";

        let output = run_resolver_from_script(
            script,
            "resolve-example.py",
            &[],
            "/bin/infigraph-mcp",
            std::path::Path::new("/home/x"),
        )
        .unwrap();

        match output {
            ResolverOutput::Ok { data } => assert_eq!(data.path, "/resolved/from-python.json"),
            other => panic!("expected Ok, got {other:?}"),
        }
    }
}
