//! Loading a transcript and saving its note, under the archive lock.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::archive;
use crate::harness::{self, Harness, LoadError, Loaded};
use crate::render;
use crate::session::Session;

#[derive(Debug)]
pub enum Outcome {
    Saved {
        path: PathBuf,
        session: Box<Session>,
    },
    Empty,
    Skipped(String),
}

#[derive(Debug)]
pub enum Error {
    Load(LoadError),
    /// Includes failing to take the lock.
    Save(archive::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Load(e) => e.fmt(f),
            Error::Save(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Load(e) => Some(e),
            Error::Save(e) => Some(e),
        }
    }
}

pub struct Request<'a> {
    pub lock: &'a Path,
    pub dir: &'a Path,
    pub transcript: &'a Path,
    pub harness: Option<Harness>,
    pub force: bool,
    pub opts: &'a render::Options,
    pub home: &'a Path,
}

/// Saves the transcript's note. The transcript is read only once the
/// lock is held: a run that read a shorter transcript must not write last.
pub fn archive(req: &Request) -> Result<Outcome, Error> {
    archive_with(req, harness::load)
}

/// `archive` with the transcript loader passed in, so a test can check
/// the lock is held at the moment of loading.
fn archive_with(
    req: &Request,
    load: impl FnOnce(&Path, Option<Harness>, bool) -> Result<Loaded, LoadError>,
) -> Result<Outcome, Error> {
    let lock = archive::lock(req.lock).map_err(Error::Save)?;
    let loaded = load(req.transcript, req.harness, req.force).map_err(Error::Load)?;
    match loaded {
        Loaded::Session(session) => {
            let path =
                archive::save(&lock, req.dir, &session, req.opts, req.home).map_err(Error::Save)?;
            Ok(Outcome::Saved { path, session })
        }
        Loaded::Empty => Ok(Outcome::Empty),
        Loaded::Skipped(reason) => Ok(Outcome::Skipped(reason)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::tz::TimeZone;

    use super::*;
    use crate::lock;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/claude/basic.jsonl"
    );

    fn opts() -> render::Options {
        render::Options {
            thinking: true,
            tool_output: true,
            max_lines: 100,
            tz: TimeZone::UTC,
        }
    }

    #[test]
    fn saves_a_session() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = opts();
        let req = Request {
            lock: &tmp.path().join("archive.lock"),
            dir: tmp.path(),
            transcript: Path::new(FIXTURE),
            harness: None,
            force: false,
            opts: &opts,
            home: tmp.path(),
        };
        let Outcome::Saved { path, session } = archive(&req).unwrap() else {
            panic!("expected a saved note");
        };
        assert!(path.starts_with(tmp.path().join("Claude/sample")));
        assert_eq!(session.id, "5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e");
    }

    #[test]
    fn reports_a_load_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = opts();
        let req = Request {
            lock: &tmp.path().join("archive.lock"),
            dir: tmp.path(),
            transcript: &tmp.path().join("missing.jsonl"),
            harness: None,
            force: false,
            opts: &opts,
            home: tmp.path(),
        };
        assert!(matches!(archive(&req), Err(Error::Load(LoadError::Io(_)))));
    }

    #[test]
    fn loads_only_once_the_lock_is_held() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = opts();
        let lock_path = tmp.path().join("archive.lock");
        let req = Request {
            lock: &lock_path,
            dir: tmp.path(),
            transcript: Path::new(FIXTURE),
            harness: None,
            force: false,
            opts: &opts,
            home: tmp.path(),
        };
        let mut probed = false;
        let outcome = archive_with(&req, |path, h, force| {
            probed = true;
            assert!(
                matches!(
                    lock::acquire(&lock_path, Duration::ZERO),
                    Err(lock::Error::Locked)
                ),
                "loaded without the lock"
            );
            harness::load(path, h, force)
        });
        assert!(probed);
        assert!(matches!(outcome, Ok(Outcome::Saved { .. })));
    }
}
