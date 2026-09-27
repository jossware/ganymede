use jiff::Timestamp;
use serde_json::Value;

use super::json::{array_at, at, str_at, timestamp_at};
use super::{LoadError, Records};
use crate::session::{Entry, Session};

/// Top-level record types that carry no conversation content, or that are
/// read for metadata only.
const IGNORED_RECORDS: &[&str] = &[
    "session_meta",
    "turn_context",
    "response_item",
    "world_state",
    "token_usage_record",
    "inter_agent_communication_metadata",
    "compacted",
];

/// `event_msg` payload types that duplicate `item_completed` or carry no content.
const IGNORED_EVENTS: &[&str] = &[
    "token_count",
    "task_started",
    "task_complete",
    "thread_settings_applied",
    "turn_aborted",
    "user_message",
    "agent_message",
    "agent_reasoning",
];

const IGNORED_ITEMS: &[&str] = &[
    "EnteredReviewMode",
    "ExitedReviewMode",
    "ContextCompaction",
    "Extension",
    "SubAgentActivity",
];

/// Early CLI rollouts predate `item_completed` and are not supported.
const LEGACY_ORIGINATOR: &str = "codex_cli_rs";

const REQUEST_MARKER: &str = "## My request:";
const FILES_HEADING: &str = "# Files mentioned by the user:";
const BROWSER_OPEN: &str = "<in-app-browser-context";
const BROWSER_CLOSE: &str = "</in-app-browser-context>";

const INTERACTIVE_SOURCES: &[&str] = &["cli", "vscode"];

/// Why a rollout is not an interactive session, or `None` to archive it.
pub fn skip_reason(records: &Records) -> Option<String> {
    // Without session_meta there is nothing to judge; parse reports the error.
    let meta = session_meta(records)?;
    let source = at(meta, "source").unwrap_or(&Value::Null);
    if !source
        .as_str()
        .is_some_and(|s| INTERACTIVE_SOURCES.contains(&s))
    {
        return Some(non_interactive("source", source));
    }
    match at(meta, "thread_source") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "user" => None,
        Some(other) => Some(non_interactive("thread_source", other)),
    }
}

fn non_interactive(field: &str, value: &Value) -> String {
    let value = match value {
        Value::String(s) => s.clone(),
        // Subagent sources are objects keyed by the subagent kind.
        Value::Object(m) => m.keys().next().cloned().unwrap_or_default(),
        other => other.to_string(),
    };
    format!("non-interactive codex session ({field}: {value})")
}

fn session_meta(records: &Records) -> Option<&Value> {
    let first = records.values.first()?;
    (str_at(first, "type") == Some("session_meta"))
        .then(|| first.get("payload"))
        .flatten()
}

pub fn parse(records: &Records, session: &mut Session) -> Result<(), LoadError> {
    let meta =
        session_meta(records).ok_or_else(|| LoadError::Parse("missing session_meta".into()))?;
    session.id = str_at(meta, "id")
        .or_else(|| str_at(meta, "session_id"))
        .unwrap_or_default()
        .to_string();
    session.harness_version = str_at(meta, "cli_version").map(String::from);
    session.started_at = timestamp_at(meta, "timestamp").or_else(|| {
        records
            .values
            .first()
            .and_then(|r| timestamp_at(r, "timestamp"))
    });
    if let Some(cwd) = str_at(meta, "cwd") {
        session.add_cwd(cwd.into());
    }

    let mut items = 0;
    for record in records.values.iter().skip(1) {
        let ts = timestamp_at(record, "timestamp");
        match str_at(record, "type").unwrap_or_default() {
            "turn_context" => {
                if let Some(cwd) = str_at(record, "payload.cwd") {
                    session.add_cwd(cwd.into());
                }
                if session.model.is_none() {
                    session.model = str_at(record, "payload.model").map(String::from);
                }
            }
            "event_msg" => match str_at(record, "payload.type").unwrap_or_default() {
                "item_completed" => {
                    items += 1;
                    let item = at(record, "payload.item").unwrap_or(&Value::Null);
                    map_item(item, ts, session);
                }
                t if IGNORED_EVENTS.contains(&t) => {}
                t => session.record_unhandled(&format!("event/{t}"), 1),
            },
            t if IGNORED_RECORDS.contains(&t) => {}
            t => session.record_unhandled(&format!("record/{t}"), 1),
        }
    }

    if items == 0 && str_at(meta, "originator") == Some(LEGACY_ORIGINATOR) {
        return Err(LoadError::Parse("unsupported codex rollout format".into()));
    }
    session.title_source = session.entries.iter().find_map(|e| match e {
        Entry::User { text, .. } => Some(title_source(text)),
        _ => None,
    });
    Ok(())
}

fn map_item(item: &Value, ts: Option<Timestamp>, session: &mut Session) {
    let entry = match str_at(item, "type").unwrap_or_default() {
        "UserMessage" => user_message(item, ts, session),
        "AgentMessage" => agent_message(item, ts, session),
        t if IGNORED_ITEMS.contains(&t) => None,
        t => {
            session.record_unhandled(&format!("item/{t}"), 1);
            None
        }
    };
    session.entries.extend(entry);
}

fn user_message(item: &Value, ts: Option<Timestamp>, session: &mut Session) -> Option<Entry> {
    let mut parts = Vec::new();
    for block in array_at(item, "content") {
        match str_at(block, "type").unwrap_or_default() {
            "text" => parts.push(clean_user_text(
                str_at(block, "text").unwrap_or_default(),
            )),
            "image" => parts.push("[image]".to_string()),
            t => session.record_unhandled(&format!("block/{t}"), 1),
        }
    }
    parts.retain(|p| !p.is_empty());
    (!parts.is_empty()).then(|| Entry::User {
        at: ts,
        text: parts.join("\n\n"),
    })
}

fn agent_message(item: &Value, ts: Option<Timestamp>, session: &mut Session) -> Option<Entry> {
    let mut parts = Vec::new();
    for block in array_at(item, "content") {
        match str_at(block, "type").unwrap_or_default() {
            "Text" => parts.push(str_at(block, "text").unwrap_or_default().to_string()),
            t => session.record_unhandled(&format!("block/{t}"), 1),
        }
    }
    // Question titles repeat the message text; only the options are new.
    let options: Vec<String> = array_at(item, "questions")
        .iter()
        .flat_map(|q| array_at(q, "options"))
        .filter_map(Value::as_str)
        .map(|o| format!("- {o}"))
        .collect();
    if !options.is_empty() {
        parts.push(options.join("\n"));
    }
    parts.retain(|p| !p.trim().is_empty());
    (!parts.is_empty()).then(|| Entry::Assistant {
        at: ts,
        text: parts.join("\n\n"),
    })
}

/// Strips context Codex injects into the user's message.
fn clean_user_text(text: &str) -> String {
    // Desktop wraps the request in attachments and UI state, then marks it.
    if let Some(request) = after_request_marker(text) {
        return request.trim().to_string();
    }
    strip_files_section(&strip_browser_context(text))
        .trim()
        .to_string()
}

fn after_request_marker(text: &str) -> Option<&str> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim_end() == REQUEST_MARKER {
            return Some(&text[offset..]);
        }
    }
    None
}

fn strip_browser_context(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(BROWSER_OPEN) {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find(BROWSER_CLOSE) {
            Some(end) => &rest[start + end + BROWSER_CLOSE.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// The section runs to the first blank line followed by a non-entry line.
fn strip_files_section(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == FILES_HEADING) else {
        return text.to_string();
    };
    let end = (start + 1..lines.len())
        .find(|&i| {
            lines[i].trim().is_empty()
                && lines
                    .get(i + 1)
                    .is_some_and(|next| !next.trim().is_empty() && !is_file_entry(next))
        })
        .unwrap_or(lines.len());
    lines[..start]
        .iter()
        .chain(&lines[end..])
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_file_entry(line: &str) -> bool {
    line.trim_start().starts_with(['-', '*', '#'])
}

/// A `$skill` invocation is titled by its arguments when it has any.
fn title_source(text: &str) -> String {
    if let Some(invocation) = text.strip_prefix('$')
        && let Some((skill, args)) = invocation.split_once(char::is_whitespace)
        && !skill.is_empty()
        && !args.trim().is_empty()
    {
        return args.trim().to_string();
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::harness::{Harness, Loaded, load};
    use crate::session::Entry;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(name)
    }

    fn fixture(name: &str) -> Records {
        Records::parse(&fs::read_to_string(fixture_path(name)).unwrap())
    }

    fn session(name: &str) -> Session {
        let mut s = Session::new(Harness::Codex, name.into());
        parse(&fixture(name), &mut s).unwrap();
        s
    }

    fn load_fixture(name: &str, force: bool) -> Result<Loaded, LoadError> {
        load(&fixture_path(name), None, force)
    }

    fn parse_str(text: &str) -> Result<Session, LoadError> {
        let mut s = Session::new(Harness::Codex, "t.jsonl".into());
        parse(&Records::parse(text), &mut s).map(|()| s)
    }

    fn users(s: &Session) -> Vec<&str> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn assistants(s: &Session) -> Vec<&str> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn interactive_sessions_are_kept() {
        for name in ["tui.jsonl", "desktop_chat.jsonl", "legacy_interactive.jsonl"] {
            assert_eq!(skip_reason(&fixture(name)), None, "{name}");
        }
    }

    #[test]
    fn non_interactive_sessions_are_skipped() {
        for (name, why) in [
            ("exec.jsonl", "source: exec"),
            ("subagent_review.jsonl", "source: subagent"),
            ("subagent_guardian.jsonl", "source: subagent"),
            ("subagent_thread_spawn.jsonl", "source: subagent"),
            ("quick_chat.jsonl", "thread_source: avatar_quick_chat"),
            ("handoff.jsonl", "thread_source: chatgpt_handoff"),
        ] {
            assert_eq!(
                skip_reason(&fixture(name)),
                Some(format!("non-interactive codex session ({why})")),
                "{name}"
            );
        }
    }

    #[test]
    fn missing_source_is_skipped() {
        let r = Records::parse(r#"{"type":"session_meta","payload":{}}"#);
        assert_eq!(
            skip_reason(&r),
            Some("non-interactive codex session (source: null)".into())
        );
    }

    #[test]
    fn without_session_meta_nothing_is_skipped() {
        let r = Records::parse(r#"{"type":"user","sessionId":"x"}"#);
        assert_eq!(skip_reason(&r), None);
    }

    #[test]
    fn metadata_from_session_meta_and_turn_context() {
        let s = session("tui.jsonl");
        assert_eq!(s.id, "01a0b000-0000-7000-8000-000000000001");
        assert_eq!(s.harness_version.as_deref(), Some("0.157.0"));
        assert_eq!(s.started_at.unwrap().to_string(), "2026-09-15T19:42:00Z");
        assert_eq!(s.model.as_deref(), Some("gpt-6-astra"));
        assert_eq!(
            s.cwds,
            [
                PathBuf::from("/Users/user/Workspaces/sample"),
                "/Users/user/Worktrees/sample/verbose".into(),
            ]
        );
    }

    #[test]
    fn messages_keep_file_order_and_every_phase() {
        let s = session("tui.jsonl");
        assert_eq!(users(&s), ["Add a --verbose flag to the sample CLI", "Thanks"]);
        assert_eq!(
            assistants(&s),
            [
                "Looking at the argument parser first.",
                "Added `--verbose`. One test still fails to compile.",
                "You’re welcome.",
            ]
        );
    }

    #[test]
    fn entry_at_is_record_timestamp() {
        let s = session("tui.jsonl");
        let Some(Entry::User { at, .. }) = s.entries.first() else {
            panic!("expected a user entry first");
        };
        assert_eq!(at.unwrap().to_string(), "2026-09-15T19:42:06.266Z");
    }

    #[test]
    fn request_marker_wins_and_images_are_marked() {
        let s = session("desktop_chat.jsonl");
        assert_eq!(
            users(&s),
            [
                "Why is the header misaligned?\n\n[image]",
                "$code-review check the CSS change",
            ]
        );
        assert_eq!(
            s.title_source.as_deref(),
            Some("Why is the header misaligned?\n\n[image]")
        );
    }

    #[test]
    fn skill_without_args_titles_by_invocation() {
        assert_eq!(session("skill_only.jsonl").title_source.as_deref(), Some("$standup"));
    }

    #[test]
    fn title_source_rules() {
        assert_eq!(title_source("$code-review check the CSS"), "check the CSS");
        assert_eq!(title_source("$standup"), "$standup");
        assert_eq!(title_source("$standup   "), "$standup   ");
        assert_eq!(title_source("$ spaced"), "$ spaced");
        assert_eq!(title_source("plain text"), "plain text");
    }

    #[test]
    fn ignored_types_are_silent_and_unknowns_tallied() {
        let s = session("ignored_and_unknown.jsonl");
        assert_eq!(
            s.unhandled,
            BTreeMap::from([
                ("codex:event/goal_updated".to_string(), 1),
                ("codex:item/SkillInvocation".to_string(), 1),
                ("codex:record/hologram".to_string(), 1),
            ])
        );
        assert_eq!(users(&s), ["Review my branch"]);
        assert_eq!(assistants(&s), ["No findings."]);
    }

    #[test]
    fn quit_before_first_turn_is_empty() {
        assert!(matches!(load_fixture("no_turns.jsonl", false), Ok(Loaded::Empty)));
    }

    #[test]
    fn legacy_rollout_is_unsupported() {
        let err = load_fixture("legacy.jsonl", false).unwrap_err();
        assert!(
            matches!(&err, LoadError::Parse(m) if m == "unsupported codex rollout format"),
            "{err:?}"
        );
    }

    #[test]
    fn legacy_originator_with_items_parses() {
        assert!(matches!(
            load_fixture("legacy_interactive.jsonl", false),
            Ok(Loaded::Session(_))
        ));
    }

    #[test]
    fn force_archives_non_interactive() {
        assert!(matches!(load_fixture("exec.jsonl", true), Ok(Loaded::Session(_))));
    }

    #[test]
    fn malformed_mid_file_line_is_tallied() {
        let Ok(Loaded::Session(s)) = load_fixture("malformed.jsonl", false) else {
            panic!("expected a session");
        };
        assert_eq!(
            s.unhandled,
            BTreeMap::from([("codex:malformed-line".to_string(), 1)])
        );
        assert_eq!(users(&s), ["Summarise the log"]);
        assert_eq!(assistants(&s), ["Two warnings."]);
    }

    #[test]
    fn missing_session_meta_is_a_parse_error() {
        let err = parse_str(r#"{"type":"user","sessionId":"x"}"#).unwrap_err();
        assert!(
            matches!(&err, LoadError::Parse(m) if m == "missing session_meta"),
            "{err:?}"
        );
    }

    #[test]
    fn later_session_meta_is_ignored() {
        let s = parse_str(concat!(
            r#"{"type":"session_meta","payload":{"id":"a","source":"cli"}}"#, "\n",
            r#"{"type":"session_meta","payload":{"id":"b","source":"cli"}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"hi"}]}}}"#,
        ))
        .unwrap();
        assert_eq!(s.id, "a");
        assert!(s.unhandled.is_empty());
        assert_eq!(users(&s), ["hi"]);
    }

    #[test]
    fn context_only_user_message_has_no_entry() {
        let s = parse_str(concat!(
            r#"{"type":"session_meta","payload":{"id":"a","source":"cli"}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"<in-app-browser-context source=\"x\">\nstate\n</in-app-browser-context>\n"}]}}}"#,
        ))
        .unwrap();
        assert!(s.entries.is_empty());
        assert!(s.is_empty());
    }

    #[test]
    fn agent_options_are_appended_and_unknown_blocks_tallied() {
        let s = parse_str(concat!(
            r#"{"type":"session_meta","payload":{"id":"a","source":"cli"}}"#, "\n",
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"Which?"},{"type":"Chart"}],"questions":[{"title":"Which?","options":["One","Two"]}]}}}"#,
        ))
        .unwrap();
        assert_eq!(assistants(&s), ["Which?\n\n- One\n- Two"]);
        assert_eq!(
            s.unhandled,
            BTreeMap::from([("codex:block/Chart".to_string(), 1)])
        );
    }

    #[test]
    fn clean_keeps_text_after_request_marker() {
        let text = "\n# Files mentioned by the user:\n\n## a.png: /x/a.png\n\nProse.\n\n\
                    <in-app-browser-context source=\"s\">\nstate\n</in-app-browser-context>\n\n\
                    ## My request:\nWhy?\n";
        assert_eq!(clean_user_text(text), "Why?");
    }

    #[test]
    fn clean_strips_browser_context() {
        let text = "Fix it\n<in-app-browser-context source=\"s\">\nstate\n</in-app-browser-context>\nplease";
        assert_eq!(clean_user_text(text), "Fix it\n\nplease");
    }

    #[test]
    fn clean_strips_unclosed_browser_context_to_end() {
        assert_eq!(clean_user_text("Fix it\n<in-app-browser-context>\nstate"), "Fix it");
    }

    #[test]
    fn clean_strips_files_section_without_marker() {
        let text = "\n# Files mentioned by the user:\n\n## notes.md: /x/notes.md\n\n- /x/b.md\n\nSummarise these\n";
        assert_eq!(clean_user_text(text), "Summarise these");
    }

    #[test]
    fn clean_leaves_plain_text() {
        assert_eq!(clean_user_text("  plain\n\ntext  "), "plain\n\ntext");
    }
}
