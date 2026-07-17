use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Regression test for issue #9 defect 5: `--ui` workers never exit after
/// their MCP client disconnects (stdin EOF) — they sleep forever to keep the
/// web UI alive, even with no client left to serve.
#[test]
fn test_ui_worker_exits_after_stdin_closes() {
    let exe = env!("CARGO_BIN_EXE_infigraph-mcp");
    let mut child = Command::new(exe)
        .arg("--worker")
        .arg("--ui")
        .arg("--port=0")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn infigraph-mcp");

    drop(child.stdin.take());

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if !exited {
        let _ = child.kill();
    }
    let _ = child.wait();

    assert!(exited, "--ui worker did not exit within 5s of stdin closing");
}
