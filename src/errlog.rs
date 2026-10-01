//! The error log, and whether a failure earns an alert.

use std::fs;
use std::io;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Payload,
    Detect,
    Parse,
    Render,
    Write,
    Panic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: Timestamp,
    pub version: String,
    pub harness: Option<String>,
    pub event: Option<String>,
    pub session: Option<String>,
    pub transcript: Option<String>,
    pub stage: Stage,
    pub error: String,
}

impl Entry {
    pub fn new(stage: Stage, error: String) -> Entry {
        Entry {
            ts: Timestamp::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            harness: None,
            event: None,
            session: None,
            transcript: None,
            stage,
            error,
        }
    }
}

/// Up to `count` entries, newest first. Takes no lock: rotation renames
/// atomically, and a torn last line is skipped like any malformed one.
pub fn recent(log: &Path, count: usize) -> io::Result<Vec<Entry>> {
    let text = read(log)?;
    let mut entries: Vec<Entry> = parse(&text).collect();
    entries.reverse();
    entries.truncate(count);
    Ok(entries)
}

fn read(log: &Path) -> io::Result<Vec<u8>> {
    match fs::read(log) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        other => other,
    }
}

fn parse(text: &[u8]) -> impl Iterator<Item = Entry> + '_ {
    text.split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    struct Env {
        tmp: TempDir,
    }

    impl Env {
        fn new() -> Env {
            Env {
                tmp: tempfile::tempdir().unwrap(),
            }
        }

        fn log(&self) -> PathBuf {
            self.tmp.path().join("state/ganymede/errors.jsonl")
        }
    }

    fn entry(stage: Stage, session: Option<&str>, transcript: Option<&str>) -> Entry {
        Entry {
            session: session.map(String::from),
            transcript: transcript.map(String::from),
            ..Entry::new(stage, "boom".into())
        }
    }

    // --- entry ---

    #[test]
    fn serializes_keys_in_spec_order() {
        let json = serde_json::to_string(&Entry::new(Stage::Parse, "x".into())).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "ts",
                "version",
                "harness",
                "event",
                "session",
                "transcript",
                "stage",
                "error"
            ]
        );
        assert_eq!(value["stage"], "parse");
        assert_eq!(value["session"], serde_json::Value::Null);
        assert!(value["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn round_trips() {
        let e = Entry {
            error: "say \"hi\"\n\tthen \\ leave".into(),
            ..entry(Stage::Write, Some("abc"), None)
        };
        let back: Entry = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        assert_eq!(back.transcript, None);
    }

    // --- recent ---

    fn seed(env: &Env, text: &str) {
        fs::create_dir_all(env.log().parent().unwrap()).unwrap();
        fs::write(env.log(), text).unwrap();
    }

    fn line(e: &Entry) -> String {
        serde_json::to_string(e).unwrap() + "\n"
    }

    #[test]
    fn recent_is_newest_first_and_limited() {
        let env = Env::new();
        let text: String = ["a", "b", "c"]
            .map(|s| line(&entry(Stage::Parse, Some(s), None)))
            .concat();
        seed(&env, &text);
        let got: Vec<String> = recent(&env.log(), 2)
            .unwrap()
            .into_iter()
            .map(|e| e.session.unwrap())
            .collect();
        assert_eq!(got, ["c", "b"]);
    }

    #[test]
    fn recent_of_a_missing_log_is_empty() {
        let env = Env::new();
        assert!(recent(&env.log(), 20).unwrap().is_empty());
    }

    #[test]
    fn recent_skips_malformed_lines_and_unknown_stages() {
        let env = Env::new();
        let good = line(&entry(Stage::Parse, Some("a"), None));
        let future = good.replace("\"parse\"", "\"teleport\"");
        seed(&env, &format!("{good}not json\n{future}{{\"ts\":\"20"));
        let got = recent(&env.log(), 20).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].stage, Stage::Parse);
    }
}
