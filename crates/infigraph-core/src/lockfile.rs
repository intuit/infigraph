//! Lock-file mechanics shared by all infigraph locks (graph write lock,
//! and future operation/session/registry locks).
//!
//! Model: a kernel advisory flock (via `fs2`) is the source of truth for
//! "held" — flocks release automatically when the holder dies, so no
//! liveness polling is needed. The JSON identity payload written into the
//! lock file exists for diagnostics (who holds it, since when, built from
//! what) and is never trusted for liveness decisions. Conservative rule:
//! a held flock with an unreadable payload is an *unknown holder* —
//! bounded-wait then `Busy`, never broken.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// Identity payload stamped into a held lock file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub pid: u32,
    pub role: String,
    pub build_hash: String,
    /// Unix epoch seconds at acquisition.
    pub acquired_at: u64,
}

impl LockInfo {
    pub fn current(role: &str) -> Self {
        Self {
            pid: std::process::id(),
            role: role.to_string(),
            build_hash: crate::build_hash().to_string(),
            acquired_at: now_epoch_secs(),
        }
    }
}

/// Current time as Unix epoch seconds. Falls back to 0 on a clock error
/// (e.g. system time before the epoch) rather than panicking.
fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returned when a lock could not be acquired within the wait budget.
#[derive(Debug)]
pub struct Busy {
    pub lock_path: PathBuf,
    pub holder: Option<LockInfo>,
    pub waited: Duration,
}

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.holder {
            Some(h) => {
                let held_for = now_epoch_secs().saturating_sub(h.acquired_at);
                write!(
                    f,
                    "{} is locked by {} (PID {}), held {}s — waited {}s, giving up",
                    self.lock_path.display(),
                    h.role,
                    h.pid,
                    held_for,
                    self.waited.as_secs()
                )
            }
            None => write!(
                f,
                "{} is locked by an unknown holder — waited {}s, giving up",
                self.lock_path.display(),
                self.waited.as_secs()
            ),
        }
    }
}

impl std::error::Error for Busy {}

/// RAII guard for a held lock file. Releasing (drop) truncates the payload
/// then unlocks, so a cleanly-released lock file is empty.
#[derive(Debug)]
pub struct LockFile {
    file: File,
    path: PathBuf,
}

impl LockFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // Best-effort: clear payload before the flock releases so readers
        // never see a stale identity on a free lock we released cleanly.
        let _ = self.file.set_len(0);
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?)
}

fn stamp(file: &mut File, role: &str) -> Result<()> {
    let info = LockInfo::current(role);
    let json = serde_json::to_string(&info)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(json.as_bytes())?;
    file.flush()?;
    Ok(())
}

/// Best-effort read of the current holder's identity. `None` when the file
/// is missing, empty, or unparseable (old binary / mid-write).
pub fn read_holder(path: &Path) -> Option<LockInfo> {
    let mut buf = String::new();
    File::open(path).ok()?.read_to_string(&mut buf).ok()?;
    serde_json::from_str(buf.trim()).ok()
}

/// Non-blocking acquisition. `Ok(None)` when another open file description
/// holds the flock. On success the identity payload is stamped.
pub fn try_acquire(path: &Path, role: &str) -> Result<Option<LockFile>> {
    let mut file = open_lock_file(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            stamp(&mut file, role)?;
            Ok(Some(LockFile {
                file,
                path: path.to_path_buf(),
            }))
        }
        Err(ref e)
            if e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(33) =>
        {
            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!("lock error on {}: {e}", path.display())),
    }
}

/// Blocking acquisition with a wait budget. Polls `try_acquire` with
/// backoff (1ms doubling to a 500ms cap). On expiry returns a `Busy`
/// error carrying the holder identity when the payload is readable.
pub fn acquire(path: &Path, role: &str, timeout: Duration) -> Result<LockFile> {
    let start = Instant::now();
    let mut delay = Duration::from_millis(1);
    loop {
        if let Some(guard) = try_acquire(path, role)? {
            return Ok(guard);
        }
        if start.elapsed() >= timeout {
            return Err(anyhow::Error::new(Busy {
                lock_path: path.to_path_buf(),
                holder: read_holder(path),
                waited: start.elapsed(),
            }));
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        std::thread::sleep(delay.min(remaining));
        delay = (delay * 2).min(Duration::from_millis(500));
    }
}

/// Observe an existing advisory lock without creating or changing its payload.
/// A free lock returns a shared guard: retaining it prevents exclusive writers
/// during a read-only inspection. Dropping it only releases the kernel lock.
#[derive(Debug)]
pub enum Observation {
    Missing,
    Free(File),
    Held,
}

pub fn observe(path: &Path) -> std::io::Result<Observation> {
    match path.metadata() {
        Ok(meta) if !meta.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "lock is not a regular file",
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Observation::Missing),
        Err(e) => return Err(e),
        _ => {}
    }
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Observation::Missing),
        Err(e) => return Err(e),
    };
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(Observation::Free(file)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(33) => {
            Ok(Observation::Held)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    #[test]
    fn observation_preserves_payload_and_does_not_create_locks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.lock");
        assert!(matches!(observe(&path).unwrap(), Observation::Missing));
        assert!(!path.exists());
        std::fs::write(&path, b"retained identity").unwrap();
        let shared = observe(&path).unwrap();
        assert!(matches!(shared, Observation::Free(_)));
        assert!(try_acquire(&path, "test").unwrap().is_none());
        drop(shared);
        assert_eq!(std::fs::read(&path).unwrap(), b"retained identity");
        let exclusive = try_acquire(&path, "test").unwrap().unwrap();
        assert!(matches!(observe(&path).unwrap(), Observation::Held));
        drop(exclusive);
    }
    #[test]
    fn non_file_lock_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert!(observe(dir.path()).is_err());
    }
}
