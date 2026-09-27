use std::path::{Path, PathBuf};

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

/// Fronts Codex's connectors; its tool names already name the connector.
const APPS_SERVER: &str = "codex_apps";

const REQUEST_MARKER: &str = "## My request:";
const FILES_HEADING: &str = "# Files mentioned by the user:";
const BROWSER_OPEN: &str = "<in-app-browser-context";
const BROWSER_CLOSE: &str = "</in-app-browser-context>";
const REPLY_OPEN: &str = "<send_user_message_question_reply>";
const REPLY_CLOSE: &str = "</send_user_message_question_reply>";

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

    let mut turn_cwd: Option<PathBuf> = session.cwds.first().cloned();
    let mut items = 0;
    for record in records.values.iter().skip(1) {
        let ts = timestamp_at(record, "timestamp");
        match str_at(record, "type").unwrap_or_default() {
            "turn_context" => {
                if let Some(cwd) = str_at(record, "payload.cwd") {
                    session.add_cwd(cwd.into());
                    turn_cwd = Some(cwd.into());
                }
                if session.model.is_none() {
                    session.model = str_at(record, "payload.model").map(String::from);
                }
            }
            "event_msg" => match str_at(record, "payload.type").unwrap_or_default() {
                "item_completed" => {
                    items += 1;
                    let item = at(record, "payload.item").unwrap_or(&Value::Null);
                    map_item(item, ts, turn_cwd.as_deref(), session);
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

fn map_item(item: &Value, ts: Option<Timestamp>, turn_cwd: Option<&Path>, session: &mut Session) {
    let entry = match str_at(item, "type").unwrap_or_default() {
        "UserMessage" => user_message(item, ts, session),
        "AgentMessage" => agent_message(item, ts, session),
        "CommandExecution" => Some(command_execution(item, ts)),
        "FileChange" => Some(file_change(item, ts, turn_cwd)),
        "McpToolCall" => Some(mcp_tool_call(item, ts)),
        "Reasoning" => reasoning(item).map(|text| Entry::Thinking { at: ts, text }),
        "WebSearch" => Some(web_search(item, ts)),
        "ImageView" => {
            let path = file_path(str_at(item, "path").unwrap_or_default());
            Some(tool(
                item,
                ts,
                "ImageView".into(),
                path,
                String::new(),
                false,
            ))
        }
        "Plan" => Some(plan(item, ts)),
        "CollabAgentToolCall" => {
            let summary = str_at(item, "tool").unwrap_or_default().to_string();
            Some(tool(
                item,
                ts,
                "CollabAgentToolCall".into(),
                summary,
                String::new(),
                false,
            ))
        }
        t if IGNORED_ITEMS.contains(&t) => None,
        t => {
            session.record_unhandled(&format!("item/{t}"), 1);
            None
        }
    };
    session.entries.extend(entry);
}

fn tool(
    item: &Value,
    ts: Option<Timestamp>,
    name: String,
    summary: String,
    output: String,
    failed: bool,
) -> Entry {
    Entry::Tool {
        at: ts,
        name,
        summary,
        output: (!output.is_empty()).then_some(output),
        failed: failed || str_at(item, "status") == Some("failed"),
    }
}

fn command_execution(item: &Value, ts: Option<Timestamp>) -> Entry {
    let summary = match at(item, "command") {
        Some(Value::String(s)) => s.clone(),
        _ => command_summary(array_at(item, "command")),
    };
    let nonzero = at(item, "exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|c| c != 0);
    let output = str_at(item, "aggregated_output")
        .unwrap_or_default()
        .to_string();
    tool(item, ts, "Shell".into(), summary, output, nonzero)
}

/// A shell wrapper's script is the interesting part of its argv.
fn command_summary(argv: &[Value]) -> String {
    let args: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
    match args.iter().position(|a| matches!(*a, "-lc" | "-c")) {
        Some(i) if i + 1 < args.len() => args[i + 1].to_string(),
        _ => args.join(" "),
    }
}

fn file_change(item: &Value, ts: Option<Timestamp>, turn_cwd: Option<&Path>) -> Entry {
    let mut summary = Vec::new();
    let mut output = String::new();
    if let Some(changes) = at(item, "changes").and_then(Value::as_object) {
        for (path, change) in changes {
            let kind = str_at(change, "type").unwrap_or("change");
            let shown = relative(path, turn_cwd);
            summary.push(match str_at(change, "move_path") {
                Some(to) => format!("{kind} {shown} → {}", relative(to, turn_cwd)),
                None => format!("{kind} {shown}"),
            });
            let body = str_at(change, "unified_diff")
                .or_else(|| str_at(change, "content"))
                .unwrap_or_default();
            if !body.is_empty() {
                output.push_str(&format!("{shown}:\n{body}"));
                if !body.ends_with('\n') {
                    output.push('\n');
                }
            }
        }
    }
    // The failure reason has been seen in either stream.
    if str_at(item, "status") == Some("failed") {
        let reasons: Vec<&str> = ["stderr", "stdout"]
            .iter()
            .filter_map(|k| str_at(item, k))
            .filter(|s| !s.is_empty())
            .collect();
        output.push_str(&reasons.join("\n"));
    }
    tool(item, ts, "Edit".into(), summary.join(", "), output, false)
}

fn relative(path: &str, cwd: Option<&Path>) -> String {
    cwd.and_then(|cwd| Path::new(path).strip_prefix(cwd).ok())
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| path.to_string(), |p| p.display().to_string())
}

fn mcp_tool_call(item: &Value, ts: Option<Timestamp>) -> Entry {
    let server = str_at(item, "server").unwrap_or_default();
    let name = str_at(item, "tool").unwrap_or_default();
    let name = if server.is_empty() || server == APPS_SERVER {
        name.to_string()
    } else {
        format!("{server}.{name}")
    };
    let summary = at(item, "arguments")
        .filter(|a| !a.is_null())
        .map(Value::to_string)
        .unwrap_or_default();
    let output = array_at(item, "result.content")
        .iter()
        .filter_map(|b| str_at(b, "text"))
        .collect::<Vec<_>>()
        .join("\n");
    let is_error = at(item, "result.isError").and_then(Value::as_bool) == Some(true);
    tool(item, ts, name, summary, output, is_error)
}

/// Most reasoning is encrypted, leaving both text fields empty.
fn reasoning(item: &Value) -> Option<String> {
    let summary = texts(array_at(item, "summary_text"));
    let text = if summary.is_empty() {
        texts(array_at(item, "raw_content"))
    } else {
        summary
    };
    (!text.is_empty()).then_some(text)
}

/// Joins text parts given either as strings or as `{"text": ...}` objects.
fn texts(parts: &[Value]) -> String {
    parts
        .iter()
        .filter_map(|p| p.as_str().or_else(|| str_at(p, "text")))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Opening a page leaves `query` empty.
fn web_search(item: &Value, ts: Option<Timestamp>) -> Entry {
    let summary = ["query", "action.query", "action.url"]
        .iter()
        .filter_map(|p| str_at(item, p))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    tool(
        item,
        ts,
        "WebSearch".into(),
        summary.into(),
        String::new(),
        false,
    )
}

fn plan(item: &Value, ts: Option<Timestamp>) -> Entry {
    let text = str_at(item, "text").unwrap_or_default();
    let summary = text
        .lines()
        .map(|l| l.trim_start_matches('#').trim())
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    tool(item, ts, "Plan".into(), summary.into(), text.into(), false)
}

fn file_path(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        let decoded = match tail {
            [hi, lo, ..] if b == b'%' => hex(*hi).zip(hex(*lo)).map(|(h, l)| (h << 4) | l),
            _ => None,
        };
        match decoded {
            Some(byte) => {
                bytes.push(byte);
                rest = &tail[2..];
            }
            None => {
                bytes.push(b);
                rest = tail;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    char::from(b)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

fn user_message(item: &Value, ts: Option<Timestamp>, session: &mut Session) -> Option<Entry> {
    let mut parts = Vec::new();
    for block in array_at(item, "content") {
        match str_at(block, "type").unwrap_or_default() {
            "text" => parts.push(clean_user_text(str_at(block, "text").unwrap_or_default())),
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
    if let Some(answers) = question_reply_answers(text) {
        return answers;
    }
    // Desktop wraps the request in attachments and UI state, then marks it.
    if let Some(request) = after_request_marker(text) {
        return request.trim().to_string();
    }
    strip_files_section(&strip_browser_context(text))
        .trim()
        .to_string()
}

/// Desktop sends answers to the agent's questions as JSON with internal ids;
/// only the answers are the user's words.
fn question_reply_answers(text: &str) -> Option<String> {
    let json = text
        .trim()
        .strip_prefix(REPLY_OPEN)?
        .strip_suffix(REPLY_CLOSE)?;
    let replies: Vec<Value> = serde_json::from_str(json).ok()?;
    let answers: Vec<&str> = replies.iter().filter_map(|r| str_at(r, "answer")).collect();
    (!answers.is_empty()).then(|| answers.join("\n\n"))
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

/// Attachments are listed as `## <name>: <path>`. Any other line, even one
/// starting with Markdown, is the user's own text.
fn is_file_entry(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("## ")
        .is_some_and(|rest| rest.contains(": "))
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
    use serde_json::json;

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
        for name in [
            "tui.jsonl",
            "desktop_chat.jsonl",
            "legacy_interactive.jsonl",
        ] {
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
        assert_eq!(
            users(&s),
            ["Add a --verbose flag to the sample CLI", "Thanks"]
        );
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
        assert_eq!(
            session("skill_only.jsonl").title_source.as_deref(),
            Some("$standup")
        );
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
        assert!(matches!(
            load_fixture("no_turns.jsonl", false),
            Ok(Loaded::Empty)
        ));
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
        assert!(matches!(
            load_fixture("exec.jsonl", true),
            Ok(Loaded::Session(_))
        ));
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
        assert_eq!(
            clean_user_text("Fix it\n<in-app-browser-context>\nstate"),
            "Fix it"
        );
    }

    #[test]
    fn clean_strips_files_section_without_marker() {
        let text = "\n# Files mentioned by the user:\n\n## notes.md: /x/notes.md\n\n## b.md: /x/b.md\n\nSummarise these\n";
        assert_eq!(clean_user_text(text), "Summarise these");
    }

    #[test]
    fn clean_keeps_markdown_request_after_files_section() {
        for request in ["# Review these files", "- Summarise the risks", "## Plan"] {
            let text =
                format!("\n# Files mentioned by the user:\n\n## a.md: /x/a.md\n\n{request}\n");
            assert_eq!(clean_user_text(&text), request);
        }
    }

    #[test]
    fn clean_unwraps_question_replies() {
        let text = "<send_user_message_question_reply>\n\
                    [{\"questionItemId\":\"[\\\"q\\\",0]\",\"question\":\"Where?\",\"answer\":\"cobalt\"},\
                    {\"question\":\"Ready?\",\"answer\":\"yes\"}]\n\
                    </send_user_message_question_reply>";
        assert_eq!(clean_user_text(text), "cobalt\n\nyes");
    }

    #[test]
    fn clean_keeps_unparseable_question_reply() {
        let text =
            "<send_user_message_question_reply>\nnot json\n</send_user_message_question_reply>";
        assert_eq!(clean_user_text(text), text);
    }

    #[test]
    fn clean_leaves_plain_text() {
        assert_eq!(clean_user_text("  plain\n\ntext  "), "plain\n\ntext");
    }

    type ToolView<'a> = (&'a str, &'a str, Option<&'a str>, bool);

    fn tools(s: &Session) -> Vec<ToolView<'_>> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Tool {
                    name,
                    summary,
                    output,
                    failed,
                    ..
                } => Some((name.as_str(), summary.as_str(), output.as_deref(), *failed)),
                _ => None,
            })
            .collect()
    }

    fn map_one(item: Value) -> Session {
        let mut s = Session::new(Harness::Codex, "t.jsonl".into());
        map_item(&item, None, None, &mut s);
        s
    }

    #[test]
    fn tui_tools_with_native_failure_signals() {
        let s = session("tui.jsonl");
        assert_eq!(
            tools(&s),
            [
                (
                    "Shell",
                    r#"rg -n "clap" src"#,
                    Some("src/cli.rs:1:use clap::Parser;\n"),
                    false
                ),
                (
                    "Shell",
                    "cargo test --quiet",
                    Some("error[E0425]: cannot find value `verbose`\n"),
                    true
                ),
                ("Shell", "false", None, true),
                (
                    "Edit",
                    "update src/cli.rs, add tests/verbose.rs, delete src/old.rs",
                    Some(
                        "src/cli.rs:\n@@ -1 +1,2 @@\n+    verbose: bool,\ntests/verbose.rs:\n#[test]\nfn verbose() {}\n"
                    ),
                    false,
                ),
                (
                    "Edit",
                    "update README.md",
                    Some("README.md:\n@@ -1 +1 @@\npatch rejected"),
                    true
                ),
                (
                    "docs.search",
                    r#"{"query":"clap verbose flag","max_results":3}"#,
                    Some("Use ArgAction::Count."),
                    false
                ),
                (
                    "cua_repl.js",
                    r#"{"code":"throw new Error()","title":"probe"}"#,
                    None,
                    true
                ),
            ]
        );
    }

    #[test]
    fn command_summary_unwraps_shell_scripts() {
        let argv = |a: &[&str]| a.iter().map(|s| json!(s)).collect::<Vec<_>>();
        assert_eq!(
            command_summary(&argv(&["/bin/zsh", "-lc", "ls -la"])),
            "ls -la"
        );
        assert_eq!(command_summary(&argv(&["bash", "-c", "x"])), "x");
        assert_eq!(command_summary(&argv(&["cargo", "test"])), "cargo test");
        assert_eq!(command_summary(&argv(&["sh", "-c"])), "sh -c");
    }

    #[test]
    fn string_command_is_its_own_summary() {
        let s = map_one(json!({"type": "CommandExecution", "command": "ls -la", "exit_code": 0}));
        assert_eq!(tools(&s), [("Shell", "ls -la", None, false)]);
    }

    #[test]
    fn fieldless_tool_items_degrade() {
        for (item, name) in [
            (json!({"type": "CommandExecution"}), "Shell"),
            (json!({"type": "FileChange", "changes": "oops"}), "Edit"),
            (json!({"type": "McpToolCall"}), ""),
        ] {
            let s = map_one(item);
            assert_eq!(tools(&s), [(name, "", None, false)]);
        }
    }

    #[test]
    fn apps_server_is_dropped_from_mcp_names() {
        let s = map_one(json!({
            "type": "McpToolCall", "server": "codex_apps", "tool": "github.fetch_file",
            "arguments": {"path": "README.md"}, "status": "completed",
            "result": {"content": [{"type": "text", "text": "not found"}], "isError": true},
        }));
        assert_eq!(
            tools(&s),
            [(
                "github.fetch_file",
                r#"{"path":"README.md"}"#,
                Some("not found"),
                true
            )]
        );
    }

    #[test]
    fn failed_file_change_appends_stderr() {
        let s = map_one(json!({
            "type": "FileChange", "status": "failed", "stderr": "no parent dir",
            "stdout": "aborted",
            "changes": {"/a/b.rs": {"type": "add", "content": "x\n"}},
        }));
        assert_eq!(
            tools(&s),
            [(
                "Edit",
                "add /a/b.rs",
                Some("/a/b.rs:\nx\nno parent dir\naborted"),
                true
            )]
        );
    }

    #[test]
    fn relative_paths() {
        let cwd = Some(Path::new("/a/b"));
        assert_eq!(relative("/a/b/c.rs", cwd), "c.rs");
        assert_eq!(relative("/x/y.rs", cwd), "/x/y.rs");
        assert_eq!(relative("/a/b", cwd), "/a/b");
        assert_eq!(relative("/a/b/c.rs", None), "/a/b/c.rs");
    }

    fn thinking(s: &Session) -> Vec<&str> {
        s.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Thinking { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn encrypted_reasoning_is_dropped() {
        assert_eq!(
            thinking(&session("tui.jsonl")),
            ["Checking how flags are parsed"]
        );
    }

    #[test]
    fn reasoning_falls_back_to_raw_content() {
        let s = map_one(json!({
            "type": "Reasoning", "summary_text": [],
            "raw_content": [{"type": "reasoning_text", "text": "raw"}, "bare"],
        }));
        assert_eq!(thinking(&s), ["raw\n\nbare"]);
    }

    #[test]
    fn desktop_tool_items() {
        let s = session("desktop_chat.jsonl");
        assert_eq!(
            tools(&s),
            [
                ("WebSearch", "css flex header alignment", None, false),
                ("WebSearch", "https://example.com/flexbox", None, false),
                (
                    "ImageView",
                    "/Users/user/Documents/Codex/2026-09-15/sample-chat/outputs/header.png",
                    None,
                    false,
                ),
                (
                    "Plan",
                    "1. Inspect header CSS",
                    Some("1. Inspect header CSS\n2. Fix alignment"),
                    false
                ),
                ("CollabAgentToolCall", "spawn_agent", None, false),
            ]
        );
    }

    #[test]
    fn web_search_uses_action_query() {
        let s = map_one(
            json!({"type": "WebSearch", "query": "", "action": {"type": "search", "query": "rust"}}),
        );
        assert_eq!(tools(&s), [("WebSearch", "rust", None, false)]);
    }

    #[test]
    fn plan_summary_drops_heading_marks() {
        let s = map_one(json!({"type": "Plan", "text": "\n# Worktree plan\n\nBody"}));
        assert_eq!(
            tools(&s),
            [(
                "Plan",
                "Worktree plan",
                Some("\n# Worktree plan\n\nBody"),
                false
            )]
        );
    }

    #[test]
    fn file_uris_decode() {
        assert_eq!(file_path("file:///a/b%20c.png"), "/a/b c.png");
        assert_eq!(file_path("file:///a/it%E2%80%99s.png"), "/a/it’s.png");
        assert_eq!(file_path("/x/100%"), "/x/100%");
        assert_eq!(file_path("/x/%zz"), "/x/%zz");
        assert_eq!(file_path("/x/%4"), "/x/%4");
        assert_eq!(file_path("/plain"), "/plain");
    }

    #[test]
    fn edge_items() {
        let s = session("edges.jsonl");
        assert_eq!(users(&s), ["Summarise the attached notes", "Every page"]);
        assert_eq!(thinking(&s), ["Raw chain of thought"]);
        assert_eq!(
            tools(&s),
            [
                (
                    "github.fetch_file",
                    r#"{"path":"README.md"}"#,
                    Some("not found"),
                    true
                ),
                (
                    "Edit",
                    "update src/old_name.rs → src/new_name.rs",
                    Some("src/old_name.rs:\n@@ -1 +1 @@\n"),
                    false
                ),
                (
                    "ImageView",
                    "/Users/user/Workspaces/sample/shots/header bar.png",
                    None,
                    false
                ),
            ]
        );
        assert_eq!(
            assistants(&s),
            [
                "Which pages should I capture?\n\n- One of each page type\n- Every page",
                "Capturing every page."
            ]
        );
    }

    #[test]
    fn known_fixtures_tally_nothing() {
        for name in [
            "tui.jsonl",
            "desktop_chat.jsonl",
            "skill_only.jsonl",
            "edges.jsonl",
            "legacy_interactive.jsonl",
        ] {
            assert_eq!(session(name).unhandled, BTreeMap::new(), "{name}");
        }
    }

    #[test]
    fn tui_snapshot() {
        insta::assert_debug_snapshot!("tui", session("tui.jsonl"));
    }

    #[test]
    fn desktop_chat_snapshot() {
        insta::assert_debug_snapshot!("desktop_chat", session("desktop_chat.jsonl"));
    }
}
