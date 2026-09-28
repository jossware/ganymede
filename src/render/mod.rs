pub mod escape;
pub mod frontmatter;
pub mod markdown;

use jiff::tz::TimeZone;

use crate::session::Session;

pub struct Options {
    pub thinking: bool,
    pub tool_output: bool,
    /// 0 means no limit.
    pub max_lines: usize,
    /// Zone for every date and time in the note.
    pub tz: TimeZone,
}

/// The whole note: frontmatter, then body.
pub fn note(session: &Session, project: &str, worktree: Option<&str>, opts: &Options) -> String {
    let mut out = frontmatter::frontmatter(session, project, worktree, &opts.tz);
    out.push_str(&markdown::body(session, opts));
    out
}

#[cfg(test)]
mod tests {
    use jiff::tz::Offset;

    use super::*;
    use crate::harness::Harness;
    use crate::session::Entry;

    #[test]
    fn note_is_frontmatter_then_body() {
        let mut s = Session::new(Harness::Codex, "t.jsonl".into());
        s.entries = vec![Entry::User {
            at: None,
            text: "hi".into(),
        }];
        let opts = Options {
            thinking: true,
            tool_output: true,
            max_lines: 100,
            tz: TimeZone::fixed(Offset::constant(-4)),
        };
        let note = note(&s, "p", None, &opts);
        assert!(note.starts_with("---\ntype: session\n"), "{note}");
        assert!(
            note.contains("---\n> [!NOTE]\n> **User**\n>\n> hi\n"),
            "{note}"
        );
        assert!(note.ends_with("hi\n") && !note.ends_with("\n\n"), "{note}");
    }
}
