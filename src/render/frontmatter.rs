//! The YAML block at the head of a note.

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::session::Session;
use crate::title;

pub fn frontmatter(
    session: &Session,
    project: &str,
    worktree: Option<&str>,
    tz: &TimeZone,
) -> String {
    let mut out = String::from("---\ntype: session\n");
    let title = title::title(session.title_source.as_deref());
    out.push_str(&format!("title: {}\n", quote(&title)));

    let local = |at: Timestamp| at.to_zoned(tz.clone());
    let start = session.started_at.map(local);
    if let Some(start) = &start {
        let date = start.date();
        out.push_str(&format!("date: {date}\n"));
        out.push_str(&format!("journal: {}\n", quote(&format!("[[{date}]]"))));
    }
    let stamps: Vec<Timestamp> = session.entries.iter().filter_map(|e| e.at()).collect();
    if let Some(last) = stamps.last() {
        let updated = local(*last);
        out.push_str(&format!(
            "updated: {}\n",
            updated.strftime("%Y-%m-%dT%H:%M")
        ));
    }
    let days: BTreeSet<Date> = start
        .iter()
        .map(|z| z.date())
        .chain(stamps.iter().map(|at| local(*at).date()))
        .collect();
    if !days.is_empty() {
        let items: Vec<String> = days.iter().map(|d| quote(&format!("[[{d}]]"))).collect();
        out.push_str(&format!("days: [{}]\n", items.join(", ")));
    }

    field(&mut out, "project", Some(project));
    field(&mut out, "harness", Some(session.harness.id()));
    field(
        &mut out,
        "harness_version",
        session.harness_version.as_deref(),
    );
    field(&mut out, "model", session.model.as_deref());
    field(&mut out, "session", Some(&session.id));
    field(&mut out, "cc_version", session.cc_version.as_deref());
    field(&mut out, "worktree", worktree);
    let transcript = session.transcript.to_string_lossy();
    field(&mut out, "transcript", Some(&transcript));
    if !session.unhandled.is_empty() {
        let items: Vec<String> = session.unhandled.keys().map(|k| quote(k)).collect();
        out.push_str(&format!("unhandled: [{}]\n", items.join(", ")));
    }
    out.push_str("---\n");
    out
}

/// Optional fields are omitted, never written empty.
fn field(out: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        out.push_str(&format!("{key}: {}\n", scalar(value)));
    }
}

/// Bare only when YAML cannot read the value as anything but this string.
/// Dates and `date`-like values must stay bare so Bases parses them.
fn scalar(value: &str) -> String {
    let bytes = value.as_bytes();
    let safe = bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || b"._/:@+~-".contains(b));
    let starts_plain = bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'/');
    if safe && starts_plain && bytes.last() != Some(&b':') {
        value.to_string()
    } else {
        quote(value)
    }
}

/// YAML rejects a raw control character even inside double quotes.
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if c.is_control() => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jiff::Timestamp;
    use jiff::tz::Offset;

    use super::*;
    use crate::harness::Harness;
    use crate::session::Entry;

    fn tz() -> TimeZone {
        TimeZone::fixed(Offset::constant(-4))
    }

    fn ts(s: &str) -> Option<Timestamp> {
        Some(s.parse().unwrap())
    }

    fn assistant(at: Option<Timestamp>) -> Entry {
        Entry::Assistant {
            at,
            text: "x".into(),
        }
    }

    fn basic() -> Session {
        let mut s = Session::new(
            Harness::Claude,
            "/Users/user/.claude/projects/sample/basic.jsonl".into(),
        );
        s.id = "5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e".into();
        s.harness_version = Some("2.1.259".into());
        s.model = Some("claude-fable-5-1".into());
        s.started_at = ts("2026-09-04T02:25:00Z");
        s.title_source = Some("Add a greeting to the sample CLI".into());
        s.cc_version = Some("v2.1.259".into());
        s.entries = vec![
            Entry::User {
                at: ts("2026-09-04T02:25:00Z"),
                text: "Add a greeting to the sample CLI".into(),
            },
            assistant(ts("2026-09-04T02:25:11.507Z")),
        ];
        s
    }

    /// The value of one `key: value` line, or `None` when the key is absent.
    fn line<'a>(block: &'a str, key: &str) -> Option<&'a str> {
        block
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(": ")))
    }

    #[test]
    fn full_block_in_fixed_order() {
        let mut s = basic();
        s.unhandled = BTreeMap::from([
            ("claude:record/foo".to_string(), 1),
            ("claude:block/bar".to_string(), 2),
        ]);
        let want = "\
---
type: session
title: \"Add a greeting to the sample CLI\"
date: 2026-09-03
journal: \"[[2026-09-03]]\"
updated: 2026-09-03T22:25
days: [\"[[2026-09-03]]\"]
project: sample
harness: claude
harness_version: 2.1.259
model: claude-fable-5-1
session: 5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e
cc_version: v2.1.259
worktree: feature
transcript: /Users/user/.claude/projects/sample/basic.jsonl
unhandled: [\"claude:block/bar\", \"claude:record/foo\"]
---
";
        assert_eq!(frontmatter(&s, "sample", Some("feature"), &tz()), want);
    }

    #[test]
    fn minimal_block_omits_every_optional_field() {
        let mut s = Session::new(Harness::Codex, "t.jsonl".into());
        s.id = "abc".into();
        let want = "\
---
type: session
title: \"(no title)\"
project: p
harness: codex
session: abc
transcript: t.jsonl
---
";
        assert_eq!(frontmatter(&s, "p", None, &tz()), want);
    }

    #[test]
    fn title_is_always_quoted_and_escaped() {
        for (source, want) in [
            (Some("say \"hi\" \\ now"), "\"say \\\"hi\\\" \\\\ now\""),
            (Some("#tag first"), "\"#tag first\""),
            (Some("plain"), "\"plain\""),
            (None, "\"(no title)\""),
        ] {
            let mut s = Session::new(Harness::Claude, "t.jsonl".into());
            s.title_source = source.map(String::from);
            let fm = frontmatter(&s, "p", None, &tz());
            assert_eq!(line(&fm, "title"), Some(want), "{source:?}");
        }
    }

    #[test]
    fn control_characters_are_escaped() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.title_source = Some("fix \x1b[31mred\x1b[0m text".into());
        let fm = frontmatter(&s, "a\u{7f}b", None, &tz());
        assert_eq!(
            line(&fm, "title"),
            Some("\"fix \\x1B[31mred\\x1B[0m text\"")
        );
        assert_eq!(line(&fm, "project"), Some("\"a\\x7Fb\""));
    }

    #[test]
    fn scalars_are_bare_only_when_safe() {
        let mut s = Session::new(Harness::Claude, "/a b/c.jsonl".into());
        s.model = Some("claude:x".into());
        s.harness_version = Some("2.1.259".into());
        let fm = frontmatter(&s, "c#1", Some("@x"), &tz());
        assert_eq!(line(&fm, "transcript"), Some("\"/a b/c.jsonl\""));
        assert_eq!(line(&fm, "project"), Some("\"c#1\""));
        assert_eq!(line(&fm, "worktree"), Some("\"@x\""));
        assert_eq!(line(&fm, "model"), Some("claude:x"));
        assert_eq!(line(&fm, "harness_version"), Some("2.1.259"));
        assert_eq!(line(&fm, "session"), None);
    }

    #[test]
    fn empty_strings_are_omitted() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.model = Some(String::new());
        let fm = frontmatter(&s, "", Some(""), &tz());
        assert!(!fm.contains("project"), "{fm}");
        assert!(!fm.contains("worktree"), "{fm}");
        assert!(!fm.contains("model"), "{fm}");
    }

    #[test]
    fn days_are_distinct_local_dates_ascending() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        // 23:30 and 23:59 on 09-03 in -04:00, then 00:01 on 09-04.
        s.started_at = ts("2026-09-04T03:30:00Z");
        s.entries = vec![
            assistant(ts("2026-09-04T03:59:00Z")),
            assistant(ts("2026-09-04T04:01:00Z")),
        ];
        let fm = frontmatter(&s, "p", None, &tz());
        assert_eq!(line(&fm, "date"), Some("2026-09-03"));
        assert_eq!(line(&fm, "updated"), Some("2026-09-04T00:01"));
        assert_eq!(
            line(&fm, "days"),
            Some("[\"[[2026-09-03]]\", \"[[2026-09-04]]\"]")
        );
    }

    #[test]
    fn days_include_the_start_date() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.started_at = ts("2026-09-01T12:00:00Z");
        s.entries = vec![assistant(ts("2026-09-04T12:00:00Z"))];
        let fm = frontmatter(&s, "p", None, &tz());
        assert_eq!(
            line(&fm, "days"),
            Some("[\"[[2026-09-01]]\", \"[[2026-09-04]]\"]")
        );
    }

    #[test]
    fn updated_is_the_last_timestamped_entry() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.entries = vec![
            assistant(ts("2026-09-04T12:00:00Z")),
            assistant(ts("2026-09-04T10:00:00Z")),
            assistant(None),
        ];
        let fm = frontmatter(&s, "p", None, &tz());
        assert_eq!(line(&fm, "updated"), Some("2026-09-04T06:00"));
    }

    #[test]
    fn no_start_but_timestamped_entries() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.entries = vec![assistant(ts("2026-09-04T12:00:00Z"))];
        let fm = frontmatter(&s, "p", None, &tz());
        assert_eq!(line(&fm, "date"), None);
        assert_eq!(line(&fm, "journal"), None);
        assert_eq!(line(&fm, "updated"), Some("2026-09-04T08:00"));
        assert_eq!(line(&fm, "days"), Some("[\"[[2026-09-04]]\"]"));
    }

    #[test]
    fn no_timestamps_at_all() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.entries = vec![assistant(None)];
        let fm = frontmatter(&s, "p", None, &tz());
        for key in ["date", "journal", "updated", "days"] {
            assert_eq!(line(&fm, key), None, "{key}");
        }
    }
}
