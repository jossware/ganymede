//! The error log, and whether a failure earns an alert.

use std::borrow::Cow;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::lock;

const MAX_LEN: usize = 1024 * 1024;
const MAX_ERROR: usize = 8 * 1024;
const TRUNCATED: &str = " …(truncated)";
const MAX_BRIEF: usize = 200;
/// Entries carry session ids and transcript paths.
const LOG_MODE: u32 = 0o600;

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

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Stage::Payload => "payload",
            Stage::Detect => "detect",
            Stage::Parse => "parse",
            Stage::Render => "render",
            Stage::Write => "write",
            Stage::Panic => "panic",
        }
    }
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

#[derive(Debug, PartialEq)]
#[must_use = "an alert must be printed as a systemMessage"]
pub enum Alert {
    Yes(String),
    No,
}

/// A session id never matches a transcript path, even when the text is
/// equal.
#[derive(PartialEq)]
enum Key<'a> {
    Session(&'a str),
    Transcript(&'a str),
}

fn key(entry: &Entry) -> Option<Key<'_>> {
    match (entry.session.as_deref(), entry.transcript.as_deref()) {
        (Some(s), _) if !s.is_empty() => Some(Key::Session(s)),
        (_, Some(t)) if !t.is_empty() => Some(Key::Transcript(t)),
        _ => None,
    }
}

#[derive(Debug)]
enum Failure {
    Locked,
    Io { path: PathBuf, source: io::Error },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Locked => f.write_str("errors.lock is held by another run"),
            Failure::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl From<lock::Error> for Failure {
    fn from(e: lock::Error) -> Failure {
        match e {
            lock::Error::Locked => Failure::Locked,
            lock::Error::Io { path, source } => Failure::Io { path, source },
        }
    }
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Failure + use<> {
    let path = path.to_path_buf();
    move |source| Failure::Io { path, source }
}

/// Appends `entry` to the log and says whether to alert. Never panics:
/// the panic hook calls it.
pub fn record(log: &Path, home: &Path, entry: &Entry) -> Alert {
    record_within(log, home, entry, MAX_LEN, lock::TIMEOUT, &mut io::stderr())
}

fn record_within(
    log: &Path,
    home: &Path,
    entry: &Entry,
    max_len: usize,
    timeout: Duration,
    stderr: &mut dyn Write,
) -> Alert {
    match check_seen_and_append(log, entry, max_len, timeout) {
        Ok(true) => Alert::No,
        Ok(false) => Alert::Yes(format!(
            "ganymede: couldn't archive this session ({}: {}). Details: {}",
            entry.stage.as_str(),
            brief(&entry.error),
            tilde(log, home),
        )),
        Err(failure) => {
            // `eprintln!` panics on a closed stderr.
            let _ = writeln!(
                stderr,
                "ganymede: couldn't write {}: {failure}",
                log.display()
            );
            if let Ok(json) = serde_json::to_string(entry) {
                let _ = writeln!(stderr, "{json}");
            }
            Alert::Yes(format!(
                "ganymede: couldn't archive this session ({}: {}). The error log also failed ({}).",
                entry.stage.as_str(),
                brief(&entry.error),
                brief(&failure.to_string()),
            ))
        }
    }
}

/// Returns whether an earlier entry already alerted for this one.
fn check_seen_and_append(
    log: &Path,
    entry: &Entry,
    max_len: usize,
    timeout: Duration,
) -> Result<bool, Failure> {
    let _lock = lock::acquire(&log.with_file_name("errors.lock"), timeout)?;
    let mut text = read(log).map_err(io_at(log))?;
    let seen = key(entry).is_some_and(|k| {
        parse(&text).any(|e| e.stage == entry.stage && key(&e).as_ref() == Some(&k))
    });
    let mut line = to_line(entry).map_err(|e| io_at(log)(e.into()))?;
    let torn = text.last().is_some_and(|&b| b != b'\n');
    let needed = line.len() + usize::from(torn);
    if text.len() + needed > max_len {
        text = rotate(log, &text, max_len.saturating_sub(needed))?;
    }
    if text.last().is_some_and(|&b| b != b'\n') {
        line.insert(0, b'\n');
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(LOG_MODE)
        .open(log)
        .and_then(|mut f| f.write_all(&line))
        .map_err(io_at(log))?;
    Ok(seen)
}

/// Serializes `entry` with a trailing newline, clipping its error so one
/// entry cannot crowd the rest out of the log.
fn to_line(entry: &Entry) -> serde_json::Result<Vec<u8>> {
    let mut line = match clip(&entry.error, MAX_ERROR) {
        Cow::Borrowed(_) => serde_json::to_vec(entry)?,
        Cow::Owned(error) => serde_json::to_vec(&Entry {
            error,
            ..entry.clone()
        })?,
    };
    line.push(b'\n');
    Ok(line)
}

/// Keeps the newest whole lines that fit in `room`, at most the newer
/// half, and returns them.
fn rotate(log: &Path, text: &[u8], room: usize) -> Result<Vec<u8>, Failure> {
    let cut = (text.len() / 2).max(text.len().saturating_sub(room));
    let from = cut.saturating_sub(1);
    let kept = match text[from..].iter().position(|&b| b == b'\n') {
        Some(i) => text[from + i + 1..].to_vec(),
        None => Vec::new(),
    };
    let dir = log.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_at(dir))?;
    let tmp_path = tmp.path().to_path_buf();
    tmp.write_all(&kept).map_err(io_at(&tmp_path))?;
    tmp.persist(log).map_err(|e| io_at(log)(e.error))?;
    Ok(kept)
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

/// At most `max` bytes of `s`, cut on a char boundary and marked.
fn clip(s: &str, max: usize) -> Cow<'_, str> {
    if s.len() <= max {
        return Cow::Borrowed(s);
    }
    Cow::Owned(format!("{}{TRUNCATED}", &s[..s.floor_char_boundary(max)]))
}

/// An error as one short line, to fit in an alert.
fn brief(s: &str) -> String {
    clip(&one_line(s), MAX_BRIEF).into_owned()
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::TempDir;

    use super::*;

    const HOME: &str = "/Users/someone";

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

        fn record(&self, entry: &Entry) -> Alert {
            record_within(
                &self.log(),
                Path::new(HOME),
                entry,
                MAX_LEN,
                Duration::ZERO,
                &mut io::sink(),
            )
        }

        fn lines(&self) -> Vec<String> {
            fs::read_to_string(self.log())
                .unwrap()
                .lines()
                .map(String::from)
                .collect()
        }
    }

    fn entry(stage: Stage, session: Option<&str>, transcript: Option<&str>) -> Entry {
        Entry {
            session: session.map(String::from),
            transcript: transcript.map(String::from),
            ..Entry::new(stage, "boom".into())
        }
    }

    fn is_yes(alert: &Alert) -> bool {
        matches!(alert, Alert::Yes(_))
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
    fn recent_reads_an_omitted_optional_key_as_none() {
        let env = Env::new();
        let e = entry(Stage::Parse, Some("s"), None);
        seed(&env, &line(&e).replace("\"harness\":null,", ""));
        assert!(!fs::read_to_string(env.log()).unwrap().contains("harness"));
        assert_eq!(recent(&env.log(), 20).unwrap(), [e]);
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

    // --- dedupe ---

    #[test]
    fn same_session_and_stage_alerts_once() {
        let env = Env::new();
        assert!(is_yes(&env.record(&entry(
            Stage::Parse,
            Some("s"),
            Some("/t")
        ))));
        assert_eq!(
            env.record(&entry(Stage::Parse, Some("s"), Some("/other"))),
            Alert::No
        );
        assert_eq!(env.lines().len(), 2);
    }

    #[test]
    fn another_stage_alerts_again() {
        let env = Env::new();
        let _ = env.record(&entry(Stage::Parse, Some("s"), None));
        assert!(is_yes(&env.record(&entry(Stage::Render, Some("s"), None))));
    }

    #[test]
    fn transcript_is_the_key_without_a_session() {
        let env = Env::new();
        assert!(is_yes(&env.record(&entry(Stage::Detect, None, Some("/t")))));
        assert_eq!(
            env.record(&entry(Stage::Detect, None, Some("/t"))),
            Alert::No
        );
    }

    #[test]
    fn no_key_always_alerts() {
        let env = Env::new();
        assert!(is_yes(&env.record(&entry(Stage::Payload, None, None))));
        assert!(is_yes(&env.record(&entry(Stage::Payload, None, None))));
    }

    #[test]
    fn empty_ids_are_no_key() {
        let env = Env::new();
        assert!(is_yes(&env.record(&entry(
            Stage::Parse,
            Some(""),
            Some("")
        ))));
        assert!(is_yes(&env.record(&entry(
            Stage::Parse,
            Some(""),
            Some("")
        ))));
    }

    #[test]
    fn session_never_matches_a_transcript() {
        let env = Env::new();
        let _ = env.record(&entry(Stage::Parse, None, Some("x")));
        assert!(is_yes(&env.record(&entry(Stage::Parse, Some("x"), None))));
    }

    #[test]
    fn concurrent_records_alert_once() {
        let env = Env::new();
        let log = env.log();
        let e = entry(Stage::Parse, Some("s"), None);
        let alerts: Vec<Alert> = std::thread::scope(|scope| {
            let runs: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        record_within(
                            &log,
                            Path::new(HOME),
                            &e,
                            MAX_LEN,
                            lock::TIMEOUT,
                            &mut io::sink(),
                        )
                    })
                })
                .collect();
            runs.into_iter().map(|r| r.join().unwrap()).collect()
        });
        assert_eq!(alerts.iter().filter(|a| is_yes(a)).count(), 1);
        assert_eq!(env.lines().len(), 8);
    }

    #[test]
    fn record_skips_a_garbage_line() {
        let env = Env::new();
        seed(&env, "not json\n");
        assert!(is_yes(&env.record(&entry(Stage::Parse, Some("s"), None))));
        assert_eq!(env.record(&entry(Stage::Parse, Some("s"), None)), Alert::No);
    }

    #[test]
    fn appends_after_a_torn_line() {
        let env = Env::new();
        seed(&env, "{\"ts\":\"20");
        assert!(is_yes(&env.record(&entry(Stage::Parse, Some("s"), None))));
        assert_eq!(env.record(&entry(Stage::Parse, Some("s"), None)), Alert::No);
        assert_eq!(env.lines().len(), 3);
    }

    #[test]
    fn log_is_private_from_the_first_write() {
        let env = Env::new();
        let _ = env.record(&entry(Stage::Parse, Some("s"), None));
        let mode = fs::metadata(env.log()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }

    // --- alert text ---

    #[test]
    fn alert_names_stage_error_and_log() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let log = home.join(".local/state/ganymede/errors.jsonl");
        let e = Entry::new(Stage::Parse, "bad\n  line\tthree".into());
        assert_eq!(
            record_within(&log, home, &e, MAX_LEN, Duration::ZERO, &mut io::sink()),
            Alert::Yes(
                "ganymede: couldn't archive this session (parse: bad line three). \
                 Details: ~/.local/state/ganymede/errors.jsonl"
                    .into()
            )
        );
    }

    fn long_error() -> Entry {
        Entry {
            error: "é word\n".repeat(MAX_BRIEF),
            ..entry(Stage::Parse, None, None)
        }
    }

    fn assert_brief(alert: &Alert) {
        let Alert::Yes(text) = alert else {
            panic!("{alert:?}")
        };
        let brief = text
            .split_once("(parse: ")
            .and_then(|(_, rest)| rest.split_once(TRUNCATED))
            .expect(text)
            .0;
        assert!(
            brief.len() <= MAX_BRIEF && brief.len() > MAX_BRIEF - 2,
            "{text}"
        );
        assert!(one_line(&long_error().error).starts_with(brief));
    }

    #[test]
    fn alert_clips_a_long_error() {
        assert_brief(&Env::new().record(&long_error()));
    }

    #[test]
    fn fallback_alert_clips_a_long_error() {
        let env = Env::new();
        let _held =
            lock::acquire(&env.log().with_file_name("errors.lock"), Duration::ZERO).unwrap();
        let alert = env.record(&long_error());
        assert!(is_fallback(&alert), "{alert:?}");
        assert_brief(&alert);
    }

    #[test]
    fn tilde_matches_whole_components() {
        let home = Path::new("/Users/ryan");
        assert_eq!(
            tilde(Path::new("/Users/ryan/x/e.jsonl"), home),
            "~/x/e.jsonl"
        );
        assert_eq!(
            tilde(Path::new("/Users/ryan2/e.jsonl"), home),
            "/Users/ryan2/e.jsonl"
        );
    }

    // --- rotation ---

    fn record_small(env: &Env, e: &Entry, max_len: usize) -> Alert {
        record_within(
            &env.log(),
            Path::new(HOME),
            e,
            max_len,
            Duration::ZERO,
            &mut io::sink(),
        )
    }

    #[test]
    fn rotation_keeps_the_newer_half_on_a_line_boundary() {
        let env = Env::new();
        for s in ["a", "b", "c", "d"] {
            let _ = env.record(&entry(Stage::Parse, Some(s), None));
        }
        let before = env.lines();
        let len = fs::metadata(env.log()).unwrap().len() as usize;
        let _ = record_small(&env, &entry(Stage::Parse, Some("e"), None), len - 1);
        let after = env.lines();
        assert_eq!(
            after[..after.len() - 1],
            before[before.len() - after.len() + 1..]
        );
        assert!(after.len() < before.len());
        assert!(after.last().unwrap().contains("\"e\""));
    }

    #[test]
    fn dedupe_sees_an_entry_rotation_drops() {
        let env = Env::new();
        for s in ["a", "b", "c", "d"] {
            let _ = env.record(&entry(Stage::Parse, Some(s), None));
        }
        let len = fs::metadata(env.log()).unwrap().len() as usize;
        assert_eq!(
            record_small(&env, &entry(Stage::Parse, Some("a"), None), len - 1),
            Alert::No
        );
        assert!(!env.lines()[0].contains("\"a\""));
    }

    #[test]
    fn rotation_keeping_a_torn_line_starts_a_new_line() {
        let env = Env::new();
        let text: String = ["a", "b", "c", "d"]
            .map(|s| line(&entry(Stage::Parse, Some(s), None)))
            .concat();
        seed(&env, &format!("{text}{{\"ts\":\"20"));
        let len = fs::metadata(env.log()).unwrap().len() as usize;
        let _ = record_small(&env, &entry(Stage::Parse, Some("e"), None), len - 1);
        let lines = env.lines();
        assert!(!lines[0].contains("\"a\""), "did not rotate");
        assert_eq!(lines[lines.len() - 2], "{\"ts\":\"20");
        assert!(lines[lines.len() - 1].contains("\"e\""));
    }

    #[test]
    fn one_huge_line_empties_the_log() {
        let env = Env::new();
        seed(&env, &"x".repeat(100));
        let _ = record_small(&env, &entry(Stage::Parse, Some("s"), None), 50);
        let lines = env.lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"s\""));
    }

    #[test]
    fn append_that_would_cross_the_cap_rotates_first() {
        let env = Env::new();
        for s in ["a", "b", "c", "d"] {
            let _ = env.record(&entry(Stage::Parse, Some(s), None));
        }
        let len = fs::metadata(env.log()).unwrap().len() as usize;
        let e = entry(Stage::Parse, Some("e"), None);
        let max_len = len + line(&e).len() - 1;
        let _ = record_small(&env, &e, max_len);
        assert!(!env.lines()[0].contains("\"a\""), "did not rotate");
        assert!(fs::metadata(env.log()).unwrap().len() as usize <= max_len);
    }

    #[test]
    fn rotation_cut_on_a_line_start_keeps_that_line() {
        let env = Env::new();
        let ts: Timestamp = "2026-10-01T00:00:00Z".parse().unwrap();
        let text: String = ["a", "b", "c", "d"]
            .map(|s| {
                line(&Entry {
                    ts,
                    ..entry(Stage::Parse, Some(s), None)
                })
            })
            .concat();
        seed(&env, &text);
        let e = entry(Stage::Parse, Some("e"), None);
        let _ = record_small(&env, &e, text.len() / 2 + line(&e).len());
        let sessions: Vec<String> = recent(&env.log(), 20)
            .unwrap()
            .into_iter()
            .map(|e| e.session.unwrap())
            .collect();
        assert_eq!(sessions, ["e", "d", "c"]);
    }

    #[test]
    fn large_entry_rotates_away_more_than_half() {
        let env = Env::new();
        let text: String = (0..10)
            .map(|i| line(&entry(Stage::Parse, Some(&i.to_string()), None)))
            .collect();
        seed(&env, &text);
        let e = Entry {
            error: "x".repeat(text.len() * 3 / 4),
            ..entry(Stage::Parse, Some("big"), None)
        };
        let _ = record_small(&env, &e, text.len());
        let len = fs::metadata(env.log()).unwrap().len() as usize;
        assert!(len <= text.len(), "{len}");
        assert!(env.lines().last().unwrap().contains("\"big\""));
    }

    #[test]
    fn torn_line_counts_toward_the_cap() {
        let env = Env::new();
        let text: String = ["a", "b", "c", "d"]
            .map(|s| line(&entry(Stage::Parse, Some(s), None)))
            .concat();
        let text = format!("{text}{{\"ts\":\"20");
        seed(&env, &text);
        let e = entry(Stage::Parse, Some("e"), None);
        let max_len = text.len() + line(&e).len();
        let _ = record_small(&env, &e, max_len);
        let len = fs::metadata(env.log()).unwrap().len() as usize;
        assert!(len <= max_len, "{len}");
    }

    #[test]
    fn entry_longer_than_the_cap_is_written_alone() {
        let env = Env::new();
        seed(&env, "{}\n");
        let _ = record_small(&env, &entry(Stage::Parse, Some("b"), None), 50);
        let lines = env.lines();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"b\""));
    }

    #[test]
    fn log_stays_within_the_cap() {
        let env = Env::new();
        let max_len = 4 * MAX_ERROR;
        for i in 0..40 {
            let e = Entry {
                error: "x".repeat(i * MAX_ERROR / 10),
                ..entry(Stage::Parse, Some(&i.to_string()), None)
            };
            let _ = record_small(&env, &e, max_len);
            let len = fs::metadata(env.log()).unwrap().len() as usize;
            assert!(len <= max_len, "entry {i}: {len}");
        }
    }

    #[test]
    fn long_error_is_truncated_on_a_char_boundary() {
        let env = Env::new();
        let error = "é".repeat(MAX_ERROR);
        let e = Entry {
            error: error.clone(),
            ..entry(Stage::Parse, Some("s"), None)
        };
        let _ = env.record(&e);
        let got = &recent(&env.log(), 1).unwrap()[0].error;
        let kept = got.strip_suffix(TRUNCATED).expect(got);
        assert!(kept.len() <= MAX_ERROR && kept.len() > MAX_ERROR - 2);
        assert!(error.starts_with(kept));
    }

    #[test]
    fn short_error_is_kept_whole() {
        let env = Env::new();
        let e = Entry {
            error: "x".repeat(MAX_ERROR),
            ..entry(Stage::Parse, Some("s"), None)
        };
        let _ = env.record(&e);
        assert_eq!(recent(&env.log(), 1).unwrap(), [e]);
    }

    // --- fallback ---

    fn is_fallback(alert: &Alert) -> bool {
        matches!(alert, Alert::Yes(m) if m.contains("The error log also failed"))
    }

    #[test]
    fn unwritable_log_alerts_every_time() {
        let env = Env::new();
        let state = env.log().parent().unwrap().to_path_buf();
        fs::create_dir_all(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o555)).unwrap();
        let e = entry(Stage::Parse, Some("s"), None);
        let first = env.record(&e);
        let second = env.record(&e);
        fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_fallback(&first), "{first:?}");
        assert!(is_fallback(&second), "{second:?}");
    }

    #[test]
    fn unreadable_log_under_the_lock_alerts_every_time() {
        let env = Env::new();
        fs::create_dir_all(env.log()).unwrap();
        let e = entry(Stage::Parse, Some("s"), None);
        for _ in 0..2 {
            let mut stderr = Vec::new();
            let alert = record_within(
                &env.log(),
                Path::new(HOME),
                &e,
                MAX_LEN,
                Duration::ZERO,
                &mut stderr,
            );
            assert!(is_fallback(&alert), "{alert:?}");
            let text = String::from_utf8(stderr).unwrap();
            let log = env.log().display().to_string();
            assert!(
                text.starts_with(&format!("ganymede: couldn't write {log}: {log}: ")),
                "{text}"
            );
        }
    }

    #[test]
    fn held_lock_falls_back() {
        let env = Env::new();
        let _held =
            lock::acquire(&env.log().with_file_name("errors.lock"), Duration::ZERO).unwrap();
        let alert = env.record(&entry(Stage::Parse, Some("s"), None));
        assert!(is_fallback(&alert), "{alert:?}");
        assert!(!env.log().exists());
    }

    #[test]
    fn fallback_writes_the_failure_and_the_entry_to_stderr() {
        let env = Env::new();
        let _held =
            lock::acquire(&env.log().with_file_name("errors.lock"), Duration::ZERO).unwrap();
        let e = entry(Stage::Parse, Some("s"), None);
        let mut stderr = Vec::new();
        let _ = record_within(
            &env.log(),
            Path::new(HOME),
            &e,
            MAX_LEN,
            Duration::ZERO,
            &mut stderr,
        );
        let text = String::from_utf8(stderr).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(
            lines[0],
            format!(
                "ganymede: couldn't write {}: errors.lock is held by another run",
                env.log().display()
            )
        );
        assert_eq!(serde_json::from_str::<Entry>(lines[1]).unwrap(), e);
    }
}
