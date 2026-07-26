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
/// Windows: `OpenProcess` + zero-timeout `WaitForSingleObject`; a handle we
/// can't open for a reason other than "no such process" is treated as alive
/// so a healthy worker is never killed spuriously.
/// Other platforms: conservatively returns `true`.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // pid 0 would signal the whole process group, and values that
        // don't fit pid_t would wrap negative (group/broadcast semantics) —
        // neither is a valid single-process PID.
        let Ok(pid_t) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid_t <= 0 {
            return false;
        }
        let res = unsafe { libc::kill(pid_t, 0) };
        if res == 0 {
            return true;
        }
        // EPERM: process exists but we can't signal it — still alive.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_TIMEOUT,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };

        if pid == 0 {
            return false;
        }
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            // ERROR_INVALID_PARAMETER: no such process. Anything else
            // (e.g. access denied) means it exists but is inaccessible —
            // err on the side of "alive" so we never exit spuriously.
            return unsafe { GetLastError() } != ERROR_INVALID_PARAMETER;
        }
        // Zero-timeout wait: WAIT_TIMEOUT ⇒ still running; WAIT_OBJECT_0
        // (or failure) ⇒ terminated.
        let res = unsafe { WaitForSingleObject(handle, 0) };
        unsafe { CloseHandle(handle) };
        res == WAIT_TIMEOUT
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Returns the current parent PID on Unix, `None` elsewhere.
fn current_ppid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(unsafe { libc::getppid() } as u32)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// If `INFIGRAPH_SUPERVISOR_PID` is set, spawn a background thread that
/// exits this process once the supervisor is gone. No-op when the env var
/// is absent (e.g. `--worker` launched directly for debugging).
///
/// When the worker is a direct child of the supervisor (the normal case),
/// the check is `getppid() != supervisor_pid`: on supervisor death the
/// kernel re-parents the worker, so this is immune to PID reuse. If the
/// worker is not a direct child (unusual debug setups), it falls back to
/// `process_alive` polling, which can in theory be fooled by PID reuse
/// but never exits a healthy process spuriously.
pub fn spawn_parent_monitor() {
    let Some(pid) = std::env::var(SUPERVISOR_PID_ENV)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
    else {
        return;
    };

    let direct_child = current_ppid() == Some(pid);

    std::thread::Builder::new()
        .name("parent-monitor".into())
        .spawn(move || loop {
            std::thread::sleep(PARENT_POLL_INTERVAL);
            let gone = if direct_child {
                // Re-parented ⇒ the supervisor died. PID-reuse-proof.
                current_ppid() != Some(pid)
            } else {
                !process_alive(pid)
            };
            if gone {
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
