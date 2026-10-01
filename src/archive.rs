//! Where a session's note lives, and writing it there.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::session::Session;
use crate::title;

const MAX_FRONTMATTER_LINES: usize = 64;
/// Enough for any frontmatter block, without reading a whole note.
const HEAD_BYTES: u64 = 16 * 1024;

#[derive(Debug)]
pub enum Error {
    /// The session id is empty, or unsafe as a filename segment.
    BadId(String),
    /// The lock was still held when the timeout ran out.
    Locked,
    /// A first write's computed path already exists.
    Taken(PathBuf),
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadId(id) if id.is_empty() => f.write_str("session has no id"),
            Error::BadId(id) => write!(f, "session id {id:?} is unsafe in a filename"),
            Error::Locked => f.write_str("archive is locked by another run"),
            Error::Taken(path) => write!(f, "{} already exists", path.display()),
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Error + use<> {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

/// The id's first 8 characters, when they are safe in a filename.
fn id8(id: &str) -> Result<String, Error> {
    let id8: String = id.chars().take(8).collect();
    let safe = !id8.is_empty()
        && id8
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if safe {
        Ok(id8)
    } else {
        Err(Error::BadId(id.to_string()))
    }
}

/// The `<date>-<slug>-<id8>.md` name of a new note.
fn file_name(session: &Session, id8: &str, tz: &TimeZone) -> String {
    let start = session
        .started_at
        .or_else(|| session.entries.iter().filter_map(|e| e.at()).min())
        .unwrap_or_else(Timestamp::now);
    let date = start.to_zoned(tz.clone()).date();
    let slug = title::slug(session.title_source.as_deref());
    if slug.is_empty() {
        format!("{date}-{id8}.md")
    } else {
        format!("{date}-{slug}-{id8}.md")
    }
}

#[derive(Debug, Default, PartialEq)]
struct Fields {
    session: Option<String>,
    project: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Found {
    path: PathBuf,
    project: Option<String>,
}

/// The note holding `id`, by newest mtime when several do. Notes are
/// searched in every project folder, since a note never moves.
fn find(harness_dir: &Path, id: &str, id8: &str) -> Result<Option<Found>, Error> {
    let folders = match fs::read_dir(harness_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_at(harness_dir)(e)),
    };
    let suffix = format!("-{id8}.md");
    let mut best: Option<(SystemTime, PathBuf, Option<String>)> = None;
    for folder in folders {
        let folder = folder.map_err(io_at(harness_dir))?.path();
        if !fs::metadata(&folder).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        for note in fs::read_dir(&folder).map_err(io_at(&folder))? {
            let note = note.map_err(io_at(&folder))?;
            let named = note
                .file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(&suffix));
            if !named || !note.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let path = note.path();
            let Ok(mtime) = note.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            let Ok(head) = head(&path) else {
                continue;
            };
            let fields = fields(&head);
            if fields.session.as_deref() != Some(id) {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|(t, p, _)| (mtime, &path) > (*t, p))
            {
                best = Some((mtime, path, fields.project));
            }
        }
    }
    Ok(best.map(|(_, path, project)| Found { path, project }))
}

fn head(path: &Path) -> io::Result<String> {
    let mut buf = Vec::new();
    File::open(path)?.take(HEAD_BYTES).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// `session:` and `project:` from a leading `---` block. Only ganymede
/// and cc2md write these notes, so this is not a YAML parser.
fn fields(text: &str) -> Fields {
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    if lines.next() != Some("---") {
        return Fields::default();
    }
    let mut found = Fields::default();
    for line in lines.take(MAX_FRONTMATTER_LINES) {
        if line == "---" {
            return found;
        }
        let Some((key, raw)) = line.split_once(": ") else {
            continue;
        };
        let slot = match key {
            "session" => &mut found.session,
            "project" => &mut found.project,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(value(raw.trim()));
        }
    }
    Fields::default()
}

fn value(raw: &str) -> String {
    let unquoted = if let Some(body) = raw.strip_prefix('"') {
        double_quoted(body)
    } else if let Some(body) = raw.strip_prefix('\'') {
        single_quoted(body)
    } else {
        None
    };
    unquoted.unwrap_or_else(|| raw.to_string())
}

/// `None` when the closing quote is missing.
fn double_quoted(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                'x' => {
                    let hex = chars
                        .as_str()
                        .get(..2)
                        .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
                    match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        Some(byte) => {
                            out.push(char::from(byte));
                            chars.nth(1);
                        }
                        None => out.push_str("\\x"),
                    }
                }
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            c => out.push(c),
        }
    }
    None
}

/// `None` when the closing quote is missing.
fn single_quoted(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            out.push(c);
        } else if chars.next_if_eq(&'\'').is_some() {
            out.push('\'');
        } else {
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;
    use std::time::Duration;

    use jiff::tz::Offset;

    use super::*;
    use crate::harness::Harness;
    use crate::session::Entry;

    const ID: &str = "5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e";

    fn set_mtime(path: &Path, secs: u64) {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(t))
            .unwrap();
    }

    fn note(session: &str, project: &str) -> String {
        format!("---\ntype: session\nproject: {project}\nsession: {session}\n---\nbody\n")
    }

    fn session_of(text: &str) -> Option<String> {
        fields(text).session
    }

    #[test]
    fn reads_bare_values() {
        assert_eq!(
            fields("---\ntitle: \"x\"\nproject: calproxy\nsession: abc\n---\nbody\n"),
            Fields {
                session: Some("abc".into()),
                project: Some("calproxy".into()),
            }
        );
    }

    #[test]
    fn unescapes_double_quotes() {
        let text = "---\nproject: \"a\\\\b\\\"c\\x07d\\x9Fe\\nf\"\n---\n";
        assert_eq!(
            fields(text).project.as_deref(),
            Some("a\\b\"c\u{7}d\u{9f}e\\nf")
        );
    }

    #[test]
    fn reads_quoted_digits_as_text() {
        assert_eq!(
            fields("---\nproject: \"2048\"\n---\n").project.as_deref(),
            Some("2048")
        );
    }

    #[test]
    fn unwraps_single_quotes() {
        assert_eq!(
            fields("---\nproject: 'it''s'\n---\n").project.as_deref(),
            Some("it's")
        );
    }

    #[test]
    fn ignores_text_after_closing_quote() {
        assert_eq!(
            fields("---\nproject: \"a\" # note\n---\n")
                .project
                .as_deref(),
            Some("a")
        );
    }

    #[test]
    fn keeps_unclosed_quote_as_written() {
        assert_eq!(
            fields("---\nproject: \"abc\n---\n").project.as_deref(),
            Some("\"abc")
        );
    }

    #[test]
    fn reads_crlf() {
        assert_eq!(
            session_of("---\r\nsession: abc\r\n---\r\n").as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn keeps_colon_space_in_value() {
        assert_eq!(
            fields("---\nproject: a: b\n---\n").project.as_deref(),
            Some("a: b")
        );
    }

    #[test]
    fn first_key_wins_and_indented_keys_do_not_count() {
        let text = "---\n  session: nested\nsession: first\nsession: second\n---\n";
        assert_eq!(session_of(text).as_deref(), Some("first"));
    }

    #[test]
    fn body_keys_do_not_count() {
        assert_eq!(session_of("---\ntype: session\n---\nsession: abc\n"), None);
    }

    #[test]
    fn needs_opening_and_closing_lines() {
        assert_eq!(session_of("session: abc\n"), None);
        assert_eq!(session_of("\n---\nsession: abc\n---\n"), None);
        assert_eq!(session_of("---\nsession: abc\n"), None);
        let long = format!("---\nsession: abc\n{}---\n", "x: y\n".repeat(63));
        assert_eq!(session_of(&long), None);
        let fits = format!("---\nsession: abc\n{}---\n", "x: y\n".repeat(62));
        assert_eq!(session_of(&fits).as_deref(), Some("abc"));
    }

    // --- id8 and file name ---

    #[test]
    fn id8_takes_eight_safe_characters() {
        assert_eq!(id8(ID).unwrap(), "5f0c2a1e");
        assert_eq!(id8("ab_-9").unwrap(), "ab_-9");
        for bad in ["", "ab/cd1234", "..abcdef", "é1234567", "abc d123"] {
            assert!(
                matches!(id8(bad), Err(Error::BadId(id)) if id == bad),
                "{bad:?}"
            );
        }
    }

    fn tz() -> TimeZone {
        TimeZone::fixed(Offset::constant(-4))
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn user(at: Option<Timestamp>) -> Entry {
        Entry::User {
            at,
            text: "Fix the archive".into(),
        }
    }

    #[test]
    fn file_name_uses_local_start_date() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.title_source = Some("Fix the archive".into());
        s.started_at = Some(ts("2026-09-04T02:25:00Z"));
        assert_eq!(
            file_name(&s, "5f0c2a1e", &tz()),
            "2026-09-03-fix-the-archive-5f0c2a1e.md"
        );
        s.title_source = None;
        assert_eq!(file_name(&s, "5f0c2a1e", &tz()), "2026-09-03-5f0c2a1e.md");
    }

    #[test]
    fn file_name_falls_back_to_earliest_entry_then_today() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.title_source = Some("Fix the archive".into());
        s.entries = vec![
            user(Some(ts("2026-09-06T12:00:00Z"))),
            user(None),
            user(Some(ts("2026-09-05T12:00:00Z"))),
        ];
        assert_eq!(
            file_name(&s, "5f0c2a1e", &tz()),
            "2026-09-05-fix-the-archive-5f0c2a1e.md"
        );
        s.entries = vec![user(None)];
        let today = || Timestamp::now().to_zoned(tz()).date();
        let before = today();
        let name = file_name(&s, "5f0c2a1e", &tz());
        let after = today();
        assert!(
            [before, after]
                .iter()
                .any(|d| name == format!("{d}-fix-the-archive-5f0c2a1e.md")),
            "{name}"
        );
    }

    // --- lookup ---

    fn put(dir: &Path, rel: &str, text: &str) -> PathBuf {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn find_misses_without_harness_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            find(&tmp.path().join("Claude"), ID, "5f0c2a1e").unwrap(),
            None
        );
    }

    #[test]
    fn find_searches_every_project_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        put(
            h,
            "alpha/2026-09-01-x-5f0c2a1e.md",
            &note("someone-else", "alpha"),
        );
        let want = put(
            h,
            ".dotfiles/2026-09-01-y-5f0c2a1e.md",
            &note(ID, "dotfiles"),
        );
        put(h, ".DS_Store", "junk");
        assert_eq!(
            find(h, ID, "5f0c2a1e").unwrap(),
            Some(Found {
                path: want,
                project: Some("dotfiles".into()),
            })
        );
    }

    #[test]
    fn find_skips_non_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        put(
            h,
            "a/2026-09-01-x-5f0c2a1e.sync-conflict-20260901-120000-ABCDEFG.md",
            &note(ID, "a"),
        );
        put(h, "a/2026-09-01-x-5f0c2a1e.md", "no frontmatter\n");
        put(
            h,
            "b/2026-09-01-x-5f0c2a1e.md",
            &format!("---\nsession: {ID}\n"),
        );
        put(h, "c/2026-09-01-x-5f0c2a1e.txt", &note(ID, "c"));
        fs::create_dir_all(h.join("d/2026-09-01-x-5f0c2a1e.md")).unwrap();
        assert_eq!(find(h, ID, "5f0c2a1e").unwrap(), None);
    }

    #[test]
    fn find_prefers_newest_then_greatest_path() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        let old = put(h, "a/2026-09-01-x-5f0c2a1e.md", &note(ID, "a"));
        let new = put(h, "b/2026-09-01-y-5f0c2a1e.md", &note(ID, "b"));
        set_mtime(&old, 2_000);
        set_mtime(&new, 1_000);
        assert_eq!(find(h, ID, "5f0c2a1e").unwrap().unwrap().path, old);
        set_mtime(&old, 1_000);
        assert_eq!(find(h, ID, "5f0c2a1e").unwrap().unwrap().path, new);
    }

    #[test]
    fn find_follows_folder_symlinks_but_not_note_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path().join("Claude");
        let elsewhere = put(
            tmp.path(),
            "elsewhere/2026-09-01-x-5f0c2a1e.md",
            &note(ID, "e"),
        );
        fs::create_dir_all(&h).unwrap();
        std::os::unix::fs::symlink(elsewhere.parent().unwrap(), h.join("linked")).unwrap();
        assert_eq!(
            find(&h, ID, "5f0c2a1e").unwrap().unwrap().path,
            h.join("linked/2026-09-01-x-5f0c2a1e.md")
        );
        fs::remove_file(h.join("linked")).unwrap();
        fs::create_dir_all(h.join("a")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, h.join("a/2026-09-01-x-5f0c2a1e.md")).unwrap();
        assert_eq!(find(&h, ID, "5f0c2a1e").unwrap(), None);
    }

    #[test]
    fn find_reads_only_the_head_of_a_large_note() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        // An odd-length prefix puts the 16 KiB cut inside a two-byte character.
        let body = format!("x{}", "é".repeat(600_000));
        let path = put(
            h,
            "a/2026-09-01-x-5f0c2a1e.md",
            &format!("{}{body}", note(ID, "a")),
        );
        assert_eq!(find(h, ID, "5f0c2a1e").unwrap().unwrap().path, path);
        // One replacement character stands in for the cut byte.
        assert!(head(&path).unwrap().len() <= HEAD_BYTES as usize + 2);
    }
}
