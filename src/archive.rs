//! Where a session's note lives, and writing it there.

use std::fmt;
use std::fs::{self, File, Permissions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::lock;
use crate::project;
use crate::render;
use crate::session::Session;
use crate::title;

const MAX_FRONTMATTER_LINES: usize = 64;
const TEMP_PREFIX: &str = ".ganymede-";
const TEMP_SUFFIX: &str = ".tmp";
const NEW_NOTE_MODE: u32 = 0o644;

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

/// Writes the session's note under `dir` and returns its path. Holding
/// `_lock` is the caller's proof that no other run is mid-save.
pub fn save(
    _lock: &Lock,
    dir: &Path,
    session: &Session,
    opts: &render::Options,
    home: &Path,
) -> Result<PathBuf, Error> {
    let id8 = id8(&session.id)?;
    // A missing vault or a mistyped --dir must not grow an unsynced archive.
    if !dir.is_dir() {
        return Err(io_at(dir)(io::ErrorKind::NotFound.into()));
    }
    let harness_dir = dir.join(session.harness.display());
    let found = find(&harness_dir, &session.id, &id8)?;
    let pinned = found.as_ref().and_then(|f| f.project.as_deref());
    let id = project::identify(&session.cwds, pinned, home);
    let text = render::note(session, &id.name, id.worktree.as_deref(), opts);
    match found {
        Some(found) => {
            let mode = fs::metadata(&found.path)
                .map_err(io_at(&found.path))?
                .permissions()
                .mode();
            write(&found.path, &text, Some(mode & 0o7777))?;
            Ok(found.path)
        }
        None => {
            let folder = harness_dir.join(&id.name);
            fs::create_dir_all(&folder).map_err(io_at(&folder))?;
            let path = folder.join(file_name(session, &id8, &opts.tz));
            write(&path, &text, None)?;
            Ok(path)
        }
    }
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
            if !named {
                continue;
            }
            let path = note.path();
            // A candidate that cannot be read may be this session's note;
            // writing a new one would duplicate it. NotFound is a file
            // deleted since the listing.
            let read = note.file_type().and_then(|t| {
                if !t.is_file() {
                    return Ok(None);
                }
                let mtime = note.metadata()?.modified()?;
                Ok(Some((mtime, head(&path)?)))
            });
            let (mtime, head) = match read {
                Ok(Some(read)) => read,
                Ok(None) => continue,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_at(&path)(e)),
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

/// A note's lines up to its frontmatter's closing line, so a long body is
/// never read.
fn head(path: &Path) -> io::Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut out = String::new();
    let mut line = Vec::new();
    for n in 0..=MAX_FRONTMATTER_LINES {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        out.push_str(&text);
        let bare = text.trim_end_matches(['\n', '\r']);
        if (n == 0 && bare != "---") || (n > 0 && bare == "---") {
            break;
        }
    }
    Ok(out)
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

/// The archive lock, held until dropped. Take it before loading the
/// transcript: a session loaded outside the lock can still be saved after
/// a newer one, and its stale render would win.
#[derive(Debug)]
#[must_use = "dropping it releases the lock"]
pub struct Lock {
    _inner: lock::Lock,
}

/// Waits up to `lock::TIMEOUT` for the lock at `path`.
pub fn lock(path: &Path) -> Result<Lock, Error> {
    lock_within(path, lock::TIMEOUT)
}

fn lock_within(path: &Path, timeout: Duration) -> Result<Lock, Error> {
    match lock::acquire(path, timeout) {
        Ok(inner) => Ok(Lock { _inner: inner }),
        Err(lock::Error::Locked) => Err(Error::Locked),
        Err(lock::Error::Io { path, source }) => Err(Error::Io { path, source }),
    }
}

/// Replaces `target` atomically. `mode` is the rewritten note's; `None`
/// marks a first write, which never replaces an existing file.
fn write(target: &Path, text: &str, mode: Option<u32>) -> Result<(), Error> {
    let dir = target.parent().unwrap_or(Path::new("."));
    sweep(dir);
    let mut tmp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .suffix(TEMP_SUFFIX)
        .tempfile_in(dir)
        .map_err(io_at(dir))?;
    let tmp_path = tmp.path().to_path_buf();
    let file = tmp.as_file_mut();
    let perms = Permissions::from_mode(mode.unwrap_or(NEW_NOTE_MODE));
    file.write_all(text.as_bytes())
        .and_then(|()| file.set_permissions(perms))
        .and_then(|()| file.sync_all())
        .map_err(io_at(&tmp_path))?;
    let persisted = match mode {
        Some(_) => tmp.persist(target),
        None => tmp.persist_noclobber(target),
    };
    match persisted {
        Ok(_) => Ok(()),
        Err(e) if mode.is_none() && e.error.kind() == io::ErrorKind::AlreadyExists => {
            Err(Error::Taken(target.to_path_buf()))
        }
        Err(e) => Err(io_at(target)(e.error)),
    }
}

/// Only a killed run leaves a temp file behind: a live one holds the lock.
fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(TEMP_PREFIX) && n.ends_with(TEMP_SUFFIX));
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;
    use std::slice;
    use std::time::Instant;

    use jiff::tz::Offset;
    use tempfile::TempDir;

    use super::*;
    use crate::harness::Harness;
    use crate::session::Entry;

    const ID: &str = "5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e";

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

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
        let body = "é".repeat(600_000);
        let path = put(
            h,
            "a/2026-09-01-x-5f0c2a1e.md",
            &format!("{}{body}", note(ID, "a")),
        );
        assert_eq!(find(h, ID, "5f0c2a1e").unwrap().unwrap().path, path);
        assert_eq!(
            head(&path).unwrap(),
            format!("---\ntype: session\nproject: a\nsession: {ID}\n---\n")
        );
    }

    #[test]
    fn find_reads_a_long_frontmatter_line() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        let long = format!("unhandled: [\"{}\"]", "x".repeat(2 * 1024 * 1024));
        let text = format!("---\nsession: {ID}\n{long}\nproject: a\n---\nbody\n");
        let path = put(h, "a/2026-09-01-x-5f0c2a1e.md", &text);
        assert_eq!(
            find(h, ID, "5f0c2a1e").unwrap(),
            Some(Found {
                path,
                project: Some("a".into()),
            })
        );
    }

    #[test]
    fn find_reports_an_unreadable_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        let path = put(h, "a/2026-09-01-x-5f0c2a1e.md", &note(ID, "a"));
        fs::set_permissions(&path, Permissions::from_mode(0o000)).unwrap();
        let err = find(h, ID, "5f0c2a1e").unwrap_err();
        assert!(
            matches!(&err, Error::Io { path: p, .. } if *p == path),
            "{err}"
        );
    }

    // --- lock ---

    fn hold(path: &Path) -> File {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        File::create(path).unwrap();
        let file = File::open(path).unwrap();
        fs4::FileExt::try_lock(&file).unwrap();
        file
    }

    #[test]
    fn lock_creates_its_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state/ganymede/archive.lock");
        let _lock = lock_within(&path, Duration::ZERO).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn lock_times_out_while_held() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("archive.lock");
        let held = hold(&path);
        let start = Instant::now();
        assert!(matches!(
            lock_within(&path, Duration::from_millis(120)),
            Err(Error::Locked)
        ));
        assert!(start.elapsed() >= Duration::from_millis(120));
        drop(held);
        let _lock = lock_within(&path, Duration::ZERO).unwrap();
    }

    #[test]
    fn lock_is_held_until_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("archive.lock");
        let first = lock(&path).unwrap();
        assert!(matches!(
            lock_within(&path, Duration::ZERO),
            Err(Error::Locked)
        ));
        drop(first);
        let _lock = lock_within(&path, Duration::ZERO).unwrap();
    }

    #[test]
    fn lock_reports_an_unusable_path() {
        let tmp = tempfile::tempdir().unwrap();
        let blocker = put(tmp.path(), "state", "a file, not a folder");
        let path = blocker.join("archive.lock");
        let err = lock_within(&path, Duration::ZERO).unwrap_err();
        assert!(
            matches!(&err, Error::Io { path: p, .. } if *p == blocker),
            "{err}"
        );
    }

    // --- write ---

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with('.'))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn first_write_is_0644_and_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("n.md");
        write(&path, "hello\n", None).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
        assert_eq!(mode_of(&path), 0o644);
        assert!(leftovers(tmp.path()).is_empty());
    }

    #[test]
    fn first_write_never_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let path = put(tmp.path(), "n.md", "theirs\n");
        assert!(matches!(write(&path, "ours\n", None), Err(Error::Taken(p)) if p == path));
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        assert!(leftovers(tmp.path()).is_empty());
    }

    #[test]
    fn rewrite_keeps_mode() {
        let tmp = tempfile::tempdir().unwrap();
        for mode in [0o600, 0o644] {
            let path = put(tmp.path(), "n.md", "old\n");
            fs::set_permissions(&path, Permissions::from_mode(mode)).unwrap();
            write(&path, "new\n", Some(mode)).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
            assert_eq!(mode_of(&path), mode);
        }
    }

    #[test]
    fn write_sweeps_stale_temp_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), ".ganymede-x.tmp", "stale");
        put(tmp.path(), ".other", "keep");
        write(&tmp.path().join("n.md"), "x\n", None).unwrap();
        assert_eq!(leftovers(tmp.path()), [".other"]);
    }

    // --- save ---

    struct Env {
        tmp: TempDir,
    }

    impl Env {
        fn new() -> Self {
            let env = Env {
                tmp: tempfile::tempdir().unwrap(),
            };
            fs::create_dir_all(env.archive()).unwrap();
            fs::create_dir_all(env.home()).unwrap();
            env
        }

        fn archive(&self) -> PathBuf {
            self.tmp.path().join("AI")
        }

        fn home(&self) -> PathBuf {
            self.tmp.path().join("home")
        }

        fn lock(&self) -> PathBuf {
            self.tmp.path().join("state/archive.lock")
        }

        /// A cwd outside any repository, so its basename is the project.
        fn cwd(&self, name: &str) -> PathBuf {
            let dir = self.tmp.path().join("work").join(name);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn save(&self, s: &Session) -> Result<PathBuf, Error> {
            let lock = lock(&self.lock()).unwrap();
            save(&lock, &self.archive(), s, &opts(), &self.home())
        }

        fn notes(&self) -> Vec<PathBuf> {
            let mut out = Vec::new();
            let Ok(folders) = fs::read_dir(self.archive().join("Claude")) else {
                return out;
            };
            for folder in folders {
                for note in fs::read_dir(folder.unwrap().path()).unwrap() {
                    out.push(note.unwrap().path());
                }
            }
            out.sort();
            out
        }
    }

    fn opts() -> render::Options {
        render::Options {
            thinking: true,
            tool_output: true,
            max_lines: 100,
            tz: tz(),
        }
    }

    fn session(id: &str, cwd: &Path, start: &str) -> Session {
        let mut s = Session::new(Harness::Claude, "/t/transcript.jsonl".into());
        s.id = id.into();
        s.started_at = Some(ts(start));
        s.title_source = Some("Fix the archive".into());
        s.add_cwd(cwd.into());
        s.entries = vec![user(s.started_at)];
        s
    }

    #[test]
    fn first_save_computes_the_path() {
        let env = Env::new();
        let s = session(ID, &env.cwd("alpha"), "2026-09-04T14:00:00Z");
        let path = env.save(&s).unwrap();
        assert_eq!(
            path,
            env.archive()
                .join("Claude/alpha/2026-09-04-fix-the-archive-5f0c2a1e.md")
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\nproject: alpha\n"), "{text}");
        assert!(text.contains(&format!("\nsession: {ID}\n")), "{text}");
        assert_eq!(mode_of(&path), 0o644);
    }

    #[test]
    fn rewrite_stays_put_and_keeps_project() {
        let env = Env::new();
        let first = env
            .save(&session(ID, &env.cwd("alpha"), "2026-09-04T14:00:00Z"))
            .unwrap();
        let mut later = session(ID, &env.cwd("beta"), "2026-09-04T14:00:00Z");
        later.started_at = Some(ts("2026-09-06T14:00:00Z"));
        later.entries.push(user(Some(ts("2026-09-06T15:00:00Z"))));
        assert_eq!(env.save(&later).unwrap(), first);
        assert_eq!(env.notes(), slice::from_ref(&first));
        let text = fs::read_to_string(&first).unwrap();
        assert!(text.contains("\nproject: alpha\n"), "{text}");
        assert!(text.contains("2026-09-06"), "{text}");
    }

    #[test]
    fn rewrite_finds_note_in_another_folder_with_quoted_project() {
        let env = Env::new();
        let old = put(
            &env.archive(),
            "Claude/2048/2026-08-01-old-5f0c2a1e.md",
            &note(ID, "\"2048\""),
        );
        fs::set_permissions(&old, Permissions::from_mode(0o600)).unwrap();
        let s = session(ID, &env.cwd("beta"), "2026-09-04T14:00:00Z");
        assert_eq!(env.save(&s).unwrap(), old);
        assert_eq!(env.notes(), slice::from_ref(&old));
        let text = fs::read_to_string(&old).unwrap();
        assert!(text.contains("\nproject: \"2048\"\n"), "{text}");
        assert_eq!(mode_of(&old), 0o600);
    }

    #[test]
    fn rewrite_picks_newest_duplicate() {
        let env = Env::new();
        let a = put(
            &env.archive(),
            "Claude/a/2026-09-01-x-5f0c2a1e.md",
            &note(ID, "a"),
        );
        let b = put(
            &env.archive(),
            "Claude/b/2026-09-02-y-5f0c2a1e.md",
            &note(ID, "b"),
        );
        set_mtime(&a, 1_000);
        set_mtime(&b, 2_000);
        let s = session(ID, &env.cwd("alpha"), "2026-09-04T14:00:00Z");
        assert_eq!(env.save(&s).unwrap(), b);
        assert_eq!(fs::read_to_string(&a).unwrap(), note(ID, "a"));
        assert_eq!(
            fs::metadata(&a).unwrap().modified().unwrap(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000)
        );
        assert!(fs::read_to_string(&b).unwrap().contains("\nproject: b\n"));
    }

    #[test]
    fn first_save_onto_another_sessions_note_is_taken() {
        let env = Env::new();
        let rel = "Claude/alpha/2026-09-04-fix-the-archive-5f0c2a1e.md";
        let theirs = put(&env.archive(), rel, &note("5f0c2a1e-other", "alpha"));
        let s = session(ID, &env.cwd("alpha"), "2026-09-04T14:00:00Z");
        assert!(matches!(env.save(&s), Err(Error::Taken(p)) if p == theirs));
        assert_eq!(
            fs::read_to_string(&theirs).unwrap(),
            note("5f0c2a1e-other", "alpha")
        );
    }

    #[test]
    fn rejected_saves_create_nothing() {
        let env = Env::new();
        for id in ["ab/cd1234", ""] {
            let s = session(id, &env.cwd("alpha"), "2026-09-04T14:00:00Z");
            assert!(matches!(env.save(&s), Err(Error::BadId(_))), "{id:?}");
        }
        assert!(!env.archive().join("Claude").exists());

        let missing = env.tmp.path().join("nope");
        let s = session(ID, &env.cwd("alpha"), "2026-09-04T14:00:00Z");
        let lock = lock(&env.lock()).unwrap();
        let err = save(&lock, &missing, &s, &opts(), &env.home()).unwrap_err();
        assert!(
            matches!(&err, Error::Io { path, .. } if *path == missing),
            "{err}"
        );
        assert!(!missing.exists());
    }

    #[test]
    fn save_without_cwds_files_under_unknown() {
        let env = Env::new();
        let mut s = session(ID, &env.cwd("alpha"), "2026-09-04T14:00:00Z");
        s.cwds.clear();
        assert_eq!(
            env.save(&s).unwrap(),
            env.archive()
                .join("Claude/unknown/2026-09-04-fix-the-archive-5f0c2a1e.md")
        );
    }
}
