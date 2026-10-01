//! Advisory file locks with a bounded wait.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The wait a test gives a lock it expects to be free. A child process
/// spawned by another test, such as insta's `cargo metadata`, briefly
/// holds copies of this process's descriptors, so a free lock can read
/// as held for an instant.
#[cfg(test)]
pub const FREE: Duration = Duration::from_secs(2);
const RETRY: Duration = Duration::from_millis(50);

/// Held until dropped. The OS releases it when the process exits, so a
/// crash cannot leave it stale.
#[derive(Debug)]
#[must_use = "dropping it releases the lock"]
pub struct Lock {
    _file: File,
}

#[derive(Debug)]
pub enum Error {
    /// The lock was still held when the timeout ran out.
    Locked,
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Locked => f.write_str("locked by another run"),
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Locked => None,
        }
    }
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Error + use<> {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

/// Waits up to `timeout` for an exclusive lock on `path`, creating the
/// file and its folder if missing.
pub fn acquire(path: &Path, timeout: Duration) -> Result<Lock, Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_at(parent))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(io_at(path))?;
    let deadline = Instant::now() + timeout;
    loop {
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => return Ok(Lock { _file: file }),
            Err(fs4::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(RETRY);
            }
            Err(fs4::TryLockError::WouldBlock) => return Err(Error::Locked),
            Err(fs4::TryLockError::Error(e)) => return Err(io_at(path)(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_waits_for_the_first() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x.lock");
        let first = acquire(&path, FREE).unwrap();
        assert!(matches!(acquire(&path, Duration::ZERO), Err(Error::Locked)));
        drop(first);
        let _lock = acquire(&path, FREE).unwrap();
    }

    #[test]
    fn unusable_path_names_the_blocker() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = tmp.path().join("state");
        fs::write(&blocker, "a file, not a folder").unwrap();
        let err = acquire(&blocker.join("x.lock"), Duration::ZERO).unwrap_err();
        assert!(
            matches!(&err, Error::Io { path, .. } if *path == blocker),
            "{err}"
        );
    }
}
