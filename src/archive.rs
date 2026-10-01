//! Where a session's note lives, and writing it there.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

const MAX_FRONTMATTER_LINES: usize = 64;

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

#[derive(Debug, Default, PartialEq)]
struct Fields {
    session: Option<String>,
    project: Option<String>,
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
    use super::*;

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
}
