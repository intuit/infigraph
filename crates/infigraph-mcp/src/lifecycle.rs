//! Worker/supervisor process lifecycle.
//!
//! The MCP binary runs as a supervisor that spawns itself with `--worker`.
//! If the supervisor dies abnormally (SIGKILL, crash), the worker used to
//! survive re-parented to launchd/init (PPID 1) while still holding the
//! instance lock — blocking every future MCP start until killed by hand.
//!
//! The supervisor passes its PID via `INFIGRAPH_SUPERVISOR_PID`; the worker
//! polls that PID and exits when it disappears. Stdin EOF alone is not
//! enough: the worker inherits the client's pipe (so it outlives a dead
//! supervisor while the client is up), and the `--ui`/`--serve` modes park
//! in infinite sleep loops that never read stdin at all.

use std::time::Duration;

/// Env var carrying the supervisor's PID to the `--worker` child.
pub const SUPERVISOR_PID_ENV: &str = "INFIGRAPH_SUPERVISOR_PID";

/// How often the worker checks that its supervisor is still alive.
const PARENT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Returns whether a process with the given PID currently exists.
///
/// Unix: `kill(pid, 0)` — success or `EPERM` both mean the process exists.
/// Windows: conservatively returns `true` (no orphan reaping there yet;
/// the worker still exits on stdin EOF in MCP mode).
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let res = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if res == 0 {
            return true;
        }
        // EPERM: process exists but we can't signal it — still alive.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// If `INFIGRAPH_SUPERVISOR_PID` is set, spawn a background thread that
/// exits this process once that PID is gone. No-op when the env var is
/// absent (e.g. `--worker` launched directly for debugging).
pub fn spawn_parent_monitor() {
    let Some(pid) = std::env::var(SUPERVISOR_PID_ENV)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
    else {
        return;
    };

    std::thread::Builder::new()
        .name("parent-monitor".into())
        .spawn(move || loop {
            std::thread::sleep(PARENT_POLL_INTERVAL);
            if !process_alive(pid) {
                crate::mcp_log(
                    "INFO",
                    &format!("supervisor (pid {pid}) is gone — worker exiting to avoid orphan"),
                );
                std::process::exit(0);
            }
        })
        .expect("failed to spawn parent-monitor thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_alive_true_for_self() {
        assert!(process_alive(std::process::id()));
    }

    /// Regression test for orphan workers: after a child exits and is reaped,
    /// its PID must be reported dead so the parent-monitor terminates the
    /// worker instead of leaving it re-parented to PID 1 holding the lock.
    #[cfg(unix)]
    #[test]
    fn process_alive_false_for_reaped_child() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let pid = child.id();
        child.wait().expect("wait for child");
        assert!(
            !process_alive(pid),
            "reaped child pid {pid} must be reported dead"
        );
    }
}
