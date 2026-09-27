mod claude;
mod codex;

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use clap::ValueEnum;
use serde_json::Value;

use crate::session::Session;

const DETECT_WINDOW: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    /// Lowercase name for frontmatter and `unhandled` keys.
    pub fn id(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }

    /// Name for archive folders and note headers.
    pub fn display(self) -> &'static str {
        match self {
            Harness::Claude => "Claude",
            Harness::Codex => "Codex",
        }
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Undetected,
    Parse(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "{e}"),
            LoadError::Undetected => f.write_str("unrecognised transcript format"),
            LoadError::Parse(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        LoadError::Io(e)
    }
}

/// The parseable lines of a JSONL transcript.
#[derive(Debug, Default)]
pub struct Records {
    pub values: Vec<Value>,
    /// Unparseable lines, excluding a trailing one the harness may still be writing.
    pub malformed: usize,
}

impl Records {
    pub fn parse(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let mut records = Records::default();
        for (i, line) in lines.iter().enumerate() {
            match serde_json::from_str(line) {
                Ok(value) => records.values.push(value),
                Err(_) if i + 1 == lines.len() => {}
                Err(_) => records.malformed += 1,
            }
        }
        records
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Detected {
    Harness(Harness),
    Empty,
    Unknown,
}

pub fn detect(records: &Records) -> Detected {
    let window = &records.values[..records.values.len().min(DETECT_WINDOW)];
    let Some(first) = window.first() else {
        return Detected::Empty;
    };
    // Codex first: its rule is the stricter one.
    if first.get("type").and_then(Value::as_str) == Some("session_meta") {
        return Detected::Harness(Harness::Codex);
    }
    // Claude transcripts often open with bookkeeping, so scan the whole window.
    if window.iter().any(|r| r.get("sessionId").is_some()) {
        return Detected::Harness(Harness::Claude);
    }
    if window.iter().all(claude::is_ignored_record) {
        return Detected::Empty;
    }
    Detected::Unknown
}

/// Loads a transcript. `Ok(None)` means an empty session: no note, no error.
pub fn load(path: &Path, harness: Option<Harness>) -> Result<Option<Session>, LoadError> {
    let records = Records::parse(&fs::read_to_string(path)?);
    let harness = match harness {
        Some(h) => h,
        None => match detect(&records) {
            Detected::Harness(h) => h,
            Detected::Empty => return Ok(None),
            Detected::Unknown => return Err(LoadError::Undetected),
        },
    };
    let mut session = Session::new(harness, path.to_path_buf());
    match harness {
        Harness::Claude => claude::parse(&records, &mut session)?,
        Harness::Codex => codex::parse(&records, &mut session)?,
    }
    session.record_unhandled("malformed-line", records.malformed);
    Ok((!session.is_empty()).then_some(session))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn records(lines: &[&str]) -> Records {
        Records::parse(&lines.join("\n"))
    }

    #[test]
    fn malformed_last_line_is_skipped_silently() {
        let r = records(&[r#"{"a":1}"#, "{not json", r#"{"b":2}"#, r#"{"c":"#]);
        assert_eq!(r.values.len(), 2);
        assert_eq!(r.malformed, 1);
    }

    #[test]
    fn blank_lines_are_ignored() {
        let r = records(&[r#"{"a":1}"#, "", "{trunc", "  "]);
        assert_eq!(r.values.len(), 1);
        assert_eq!(r.malformed, 0);
    }

    #[test]
    fn detects_codex_by_first_record() {
        let r = records(&[r#"{"type":"session_meta","payload":{}}"#]);
        assert_eq!(detect(&r), Detected::Harness(Harness::Codex));
    }

    #[test]
    fn codex_wins_over_session_id() {
        let r = records(&[r#"{"type":"session_meta","sessionId":"x"}"#]);
        assert_eq!(detect(&r), Detected::Harness(Harness::Codex));
    }

    #[test]
    fn detects_claude_after_bookkeeping() {
        let r = records(&[
            r#"{"type":"last-prompt"}"#,
            r#"{"type":"file-history-snapshot"}"#,
            r#"{"type":"user","sessionId":"abc"}"#,
        ]);
        assert_eq!(detect(&r), Detected::Harness(Harness::Claude));
    }

    #[test]
    fn session_meta_later_is_not_codex() {
        let r = records(&[
            r#"{"type":"user","sessionId":"abc"}"#,
            r#"{"type":"session_meta"}"#,
        ]);
        assert_eq!(detect(&r), Detected::Harness(Harness::Claude));
    }

    #[test]
    fn no_records_is_empty() {
        assert_eq!(detect(&records(&[])), Detected::Empty);
        assert_eq!(detect(&records(&["{trunc"])), Detected::Empty);
    }

    #[test]
    fn bookkeeping_only_is_empty() {
        let r = records(&[
            r#"{"type":"file-history-snapshot"}"#,
            r#"{"type":"summary"}"#,
        ]);
        assert_eq!(detect(&r), Detected::Empty);
    }

    #[test]
    fn session_id_past_window_is_not_seen() {
        let mut lines = vec![r#"{"type":"file-history-snapshot"}"#; DETECT_WINDOW];
        lines.push(r#"{"type":"user","sessionId":"abc"}"#);
        assert_eq!(detect(&records(&lines)), Detected::Empty);
    }

    #[test]
    fn unrecognised_records_are_unknown() {
        let r = records(&[r#"{"type":"file-history-snapshot"}"#, r#"{"foo":1}"#]);
        assert_eq!(detect(&r), Detected::Unknown);
    }

    #[test]
    fn fixtures_detect_as_their_harness() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for (dir, harness) in [("claude", Harness::Claude), ("codex", Harness::Codex)] {
            for entry in fs::read_dir(root.join(dir)).unwrap() {
                let path = entry.unwrap().path();
                let records = Records::parse(&fs::read_to_string(&path).unwrap());
                let want = if path.ends_with("snapshot_only.jsonl") {
                    Detected::Empty
                } else {
                    Detected::Harness(harness)
                };
                assert_eq!(detect(&records), want, "{}", path.display());
            }
        }
    }

    #[test]
    fn fixtures_count_mid_file_malformed_lines() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for dir in ["claude", "codex"] {
            let text = fs::read_to_string(root.join(dir).join("malformed.jsonl")).unwrap();
            assert_eq!(Records::parse(&text).malformed, 1, "{dir}");
        }
    }

    fn transcript(text: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f
    }

    #[test]
    fn load_empty_file_is_empty_session() {
        let f = transcript("");
        assert!(load(f.path(), None).unwrap().is_none());
    }

    #[test]
    fn load_override_skips_detection() {
        let f = transcript(r#"{"foo":1}"#);
        assert!(matches!(load(f.path(), None), Err(LoadError::Undetected)));
        assert!(!matches!(
            load(f.path(), Some(Harness::Claude)),
            Err(LoadError::Undetected)
        ));
    }

    #[test]
    fn load_missing_file_is_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load(&dir.path().join("nope.jsonl"), None).unwrap_err();
        assert!(matches!(err, LoadError::Io(_)));
    }
}
