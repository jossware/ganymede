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
    let lock = archive::lock(req.lock).map_err(Error::Save)?;
    let loaded = harness::load(req.transcript, req.harness, req.force).map_err(Error::Load)?;
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
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::thread;
    use std::time::Duration;

    use jiff::tz::TimeZone;

    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/claude/basic.jsonl"
    );
    const LATE_TURN: &str = r#"{"parentUuid":null,"isSidechain":false,"type":"user","message":{"role":"user","content":"appended while the lock was held"},"uuid":"00000000-0000-4000-8000-0000000000ff","timestamp":"2026-09-04T03:00:00.000Z","userType":"external","entrypoint":"cli","cwd":"/Users/user/Workspaces/sample","sessionId":"5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e","version":"2.1.259"}"#;

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
        let transcript = tmp.path().join("t.jsonl");
        fs::copy(FIXTURE, &transcript).unwrap();
        let lock_path = tmp.path().join("archive.lock");
        let held = archive::lock(&lock_path).unwrap();

        let dir = tmp.path().to_path_buf();
        let worker = {
            let transcript = transcript.clone();
            thread::spawn(move || {
                let opts = opts();
                let req = Request {
                    lock: &lock_path,
                    dir: &dir,
                    transcript: &transcript,
                    harness: None,
                    force: false,
                    opts: &opts,
                    home: &dir,
                };
                archive(&req).map(|outcome| match outcome {
                    Outcome::Saved { path, .. } => path,
                    other => panic!("expected a saved note, got {other:?}"),
                })
            })
        };
        // Well past the lock's 50 ms retry, so the worker is waiting on it.
        thread::sleep(Duration::from_millis(500));
        assert!(!worker.is_finished(), "worker ran without the lock");
        let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
        writeln!(file, "{LATE_TURN}").unwrap();
        drop(held);

        let note = fs::read_to_string(worker.join().unwrap().unwrap()).unwrap();
        assert!(note.contains("appended while the lock was held"));
    }
}
