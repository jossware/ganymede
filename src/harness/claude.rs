use jiff::Timestamp;
use serde_json::Value;
use std::collections::HashMap;

use super::json::{array_at, at, str_at, timestamp_at};
use super::{LoadError, Records};
use crate::session::{Entry, Session};

/// Top-level record types that carry no conversation content.
pub const IGNORED_RECORDS: &[&str] = &[
    "system",
    "attachment",
    "last-prompt",
    "queue-operation",
    "file-history-snapshot",
    "file-history-delta",
    "summary",
    "mode",
    "permission-mode",
    "progress",
    "ai-title",
    "custom-title",
    "bridge-session",
    "atis-latch",
    "pr-link",
    "cost-state",
    "agent-name",
    "frame-link",
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "continued-in",
];

/// The model Claude Code names on messages it writes itself, such as API errors.
const SYNTHETIC_MODEL: &str = "<synthetic>";

const COMPACTION: &str = "compaction";

/// Tags Claude Code injects into user messages; the user never typed them.
/// `bash-notification` is the pre-2.1 background shell notice.
const INJECTED_TAGS: &[&str] = &[
    "system-reminder",
    "local-command-caveat",
    "command-message",
    "bash-notification",
];

/// The input field that names what each tool acts on. `Task` is the old name
/// for `Agent`.
const SUMMARY_FIELDS: &[(&str, &str)] = &[
    ("Bash", "command"),
    ("Read", "file_path"),
    ("Write", "file_path"),
    ("Edit", "file_path"),
    ("NotebookEdit", "notebook_path"),
    ("Glob", "pattern"),
    ("Grep", "pattern"),
    ("WebSearch", "query"),
    ("ToolSearch", "query"),
    ("WebFetch", "url"),
    ("Agent", "description"),
    ("Task", "description"),
    ("Skill", "skill"),
    ("TaskCreate", "subject"),
    ("TaskUpdate", "taskId"),
    ("SendMessage", "to"),
];

const MCP_PREFIX: &str = "mcp__";

/// Labels command output that arrives with no command before it.
const LOCAL_COMMAND: &str = "local command";

/// Stdout tags first, so output reads stdout then stderr.
const OUTPUT_TAGS: &[&str] = &[
    "local-command-stdout",
    "bash-stdout",
    "local-command-stderr",
    "bash-stderr",
];

const TEAMMATE_OPEN: &str = "<teammate-message";
const TEAMMATE_CLOSE: &str = "</teammate-message>";
const NOTIFICATION_OPEN: &str = "<task-notification>";
const NOTIFICATION_CLOSE: &str = "</task-notification>";

/// Teammate status pings that carry no message.
const TEAMMATE_NOISE: &[&str] = &[
    "idle_notification",
    "shutdown_approved",
    "shutdown_request",
    "teammate_terminated",
];

pub fn is_ignored_record(record: &Value) -> bool {
    record
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| IGNORED_RECORDS.contains(&t))
}

pub fn parse(records: &Records, session: &mut Session) -> Result<(), LoadError> {
    let mut walk = Walk::default();
    let mut session_id_seen = false;
    let mut sidechain_id = None;
    for record in &records.values {
        let id = non_empty(record, "sessionId");
        session_id_seen |= id.is_some();
        // Subagent output reaches the main thread through tool results and
        // task notifications.
        if flag(record, "isSidechain") {
            if sidechain_id.is_none() {
                sidechain_id = id.map(String::from);
            }
            continue;
        }
        metadata(record, session);
        match str_at(record, "type").unwrap_or_default() {
            "user" if flag(record, "isMeta") => {}
            "user" => walk.user(record, session),
            "assistant" => walk.assistant(record, session),
            t if IGNORED_RECORDS.contains(&t) => {}
            t => session.record_unhandled(&format!("record/{t}"), 1),
        }
    }
    if !session_id_seen {
        return Err(LoadError::Parse("missing sessionId".into()));
    }
    // A sidechain-only file has no non-sidechain record to carry it.
    if session.id.is_empty()
        && let Some(id) = sidechain_id
    {
        session.id = id;
    }
    Ok(())
}

fn metadata(record: &Value, session: &mut Session) {
    if session.id.is_empty()
        && let Some(id) = non_empty(record, "sessionId")
    {
        session.id = id.to_string();
    }
    if session.harness_version.is_none()
        && let Some(version) = non_empty(record, "version")
    {
        session.harness_version = Some(version.to_string());
        // cc2md wrote the version with a `v`; existing notes are queried that way.
        session.cc_version = Some(format!("v{version}"));
    }
    if session.started_at.is_none() {
        session.started_at = timestamp_at(record, "timestamp");
    }
    if let Some(cwd) = non_empty(record, "cwd") {
        session.add_cwd(cwd.into());
    }
    if session.model.is_none() && str_at(record, "type") == Some("assistant") {
        session.model = non_empty(record, "message.model")
            .filter(|m| *m != SYNTHETIC_MODEL)
            .map(String::from);
    }
}

/// One block of a user message: text the user typed, or an attachment's
/// placeholder. Titles leave placeholders out.
#[derive(Clone)]
struct Piece {
    text: String,
    typed: bool,
}

impl Piece {
    fn typed(text: &str) -> Self {
        Self {
            text: text.to_string(),
            typed: true,
        }
    }

    fn placeholder(text: String) -> Self {
        Self { text, typed: false }
    }
}

#[derive(Default)]
struct Walk {
    /// Entry index of each tool call still waiting for its result.
    pending: HashMap<String, usize>,
    /// Name and summary for the next command-output record.
    last_command: Option<(String, String)>,
}

impl Walk {
    fn user(&mut self, record: &Value, session: &mut Session) {
        let ts = timestamp_at(record, "timestamp");
        let mut pieces = Vec::new();
        match at(record, "message.content") {
            Some(Value::String(s)) => pieces.push(Piece::typed(s)),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    match str_at(block, "type").unwrap_or_default() {
                        "text" => pieces.extend(str_at(block, "text").map(Piece::typed)),
                        "image" => pieces.push(Piece::placeholder("[image]".into())),
                        "document" => {
                            pieces.push(Piece::placeholder(match non_empty(block, "title") {
                                Some(title) => format!("[document: {title}]"),
                                None => "[document]".into(),
                            }))
                        }
                        "tool_result" => self.tool_result(block, session),
                        t => session.record_unhandled(&format!("block/{t}"), 1),
                    }
                }
            }
            _ => {}
        }
        pieces.retain(|p| !p.text.trim().is_empty());
        if flag(record, "isCompactSummary") {
            // It restates the conversation above it, so it is set apart from
            // the user's own turns.
            let text = join(&pieces, false);
            let text = text.trim();
            if !text.is_empty() {
                session.entries.push(Entry::AgentReport {
                    at: ts,
                    name: COMPACTION.into(),
                    text: text.into(),
                });
            }
            return;
        }
        self.user_text(&pieces, ts, session);
    }

    /// Classifies by what the text starts with, so a user who types one of
    /// these tags in prose is not misread. Commands and their output arrive
    /// as a single string, so they are recognised on the joined raw text.
    fn user_text(&mut self, pieces: &[Piece], ts: Option<Timestamp>, session: &mut Session) {
        let raw = join(pieces, false);
        let text = raw.trim();
        if let Some((name, args)) = slash_command(text) {
            let line = if args.is_empty() {
                name.clone()
            } else {
                format!("{name} {args}")
            };
            push_user(session, ts, line, &args);
            self.last_command = Some((name, args));
            return;
        }
        if text.starts_with("<bash-input>")
            && let Some(command) = inner(text, "bash-input")
        {
            let command = command.trim().to_string();
            push_user(session, ts, format!("! {command}"), "");
            self.last_command = Some(("!".into(), command));
            return;
        }
        if OUTPUT_TAGS
            .iter()
            .any(|tag| text.starts_with(&format!("<{tag}>")))
        {
            let output = OUTPUT_TAGS
                .iter()
                .filter_map(|tag| inner(text, tag))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let (name, summary) = self
                .last_command
                .clone()
                .unwrap_or_else(|| (LOCAL_COMMAND.into(), String::new()));
            // Stderr is not a failure signal.
            session.entries.push(Entry::Tool {
                at: ts,
                name,
                summary,
                output: (!output.is_empty()).then_some(output),
                failed: false,
            });
            return;
        }
        let before = session.entries.len();
        let mut rest = Vec::with_capacity(pieces.len());
        for piece in pieces {
            if !piece.typed {
                rest.push(piece.clone());
                continue;
            }
            let (reports, remaining) = agent_reports(&piece.text);
            for (name, report) in reports {
                session.entries.push(Entry::AgentReport {
                    at: ts,
                    name,
                    text: report,
                });
            }
            rest.push(Piece::typed(&remaining));
        }
        let (text, title) = clean_pieces(&rest);
        if !text.is_empty() {
            push_user(session, ts, text, &title);
        }
        // A stray output record after later conversation must not borrow a
        // stale command's name.
        if session.entries.len() > before {
            self.last_command = None;
        }
    }

    fn assistant(&mut self, record: &Value, session: &mut Session) {
        let ts = timestamp_at(record, "timestamp");
        for block in array_at(record, "message.content") {
            match str_at(block, "type").unwrap_or_default() {
                "text" => {
                    if let Some(text) = non_blank(block, "text") {
                        session.entries.push(Entry::Assistant {
                            at: ts,
                            text: text.into(),
                        });
                    }
                }
                // Most thinking blocks carry only a signature.
                "thinking" => {
                    if let Some(text) = non_blank(block, "thinking") {
                        session.entries.push(Entry::Thinking {
                            at: ts,
                            text: text.into(),
                        });
                    }
                }
                "redacted_thinking" => {}
                "tool_use" => self.tool_use(block, ts, session),
                t => session.record_unhandled(&format!("block/{t}"), 1),
            }
        }
    }

    fn tool_use(&mut self, block: &Value, ts: Option<Timestamp>, session: &mut Session) {
        let input = at(block, "input").unwrap_or(&Value::Null);
        let (name, summary) = tool_label(str_at(block, "name").unwrap_or_default(), input);
        if let Some(id) = non_empty(block, "id") {
            self.pending.insert(id.to_string(), session.entries.len());
        }
        session.entries.push(Entry::Tool {
            at: ts,
            name,
            summary,
            output: None,
            failed: false,
        });
    }

    /// Only the first result for a pending call counts; an orphan has no name
    /// or arguments to show.
    fn tool_result(&mut self, block: &Value, session: &mut Session) {
        let Some(index) = str_at(block, "tool_use_id").and_then(|id| self.pending.remove(id))
        else {
            return;
        };
        let text = result_text(block, session);
        if let Some(Entry::Tool { output, failed, .. }) = session.entries.get_mut(index) {
            *output = (!text.trim().is_empty()).then_some(text);
            *failed = flag(block, "is_error");
        }
    }
}

/// The first user turn titles the session by `title`, falling back to the
/// entry's text so a placeholder-only opener still has a source.
fn push_user(session: &mut Session, at: Option<Timestamp>, text: String, title: &str) {
    if session.title_source.is_none() {
        session.title_source = Some(if title.is_empty() {
            text.clone()
        } else {
            title.to_string()
        });
    }
    session.entries.push(Entry::User { at, text });
}

fn result_text(block: &Value, session: &mut Session) -> String {
    match at(block, "content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match str_at(part, "type").unwrap_or_default() {
                "text" => str_at(part, "text").map(String::from),
                "image" => Some("[image]".to_string()),
                "tool_reference" => None,
                t => {
                    session.record_unhandled(&format!("block/{t}"), 1);
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn tool_label(name: &str, input: &Value) -> (String, String) {
    if let Some((server, tool)) = mcp_name(name) {
        let summary = if input.is_null() {
            String::new()
        } else {
            input.to_string()
        };
        return (format!("{server}.{tool}"), summary);
    }
    let summary = match name {
        "AskUserQuestion" => array_at(input, "questions")
            .first()
            .and_then(|q| str_at(q, "question")),
        _ => SUMMARY_FIELDS
            .iter()
            .find(|(tool, _)| *tool == name)
            .and_then(|(_, field)| str_at(input, field)),
    };
    (name.to_string(), summary.unwrap_or_default().to_string())
}

/// Server names can contain `__`, so the tool is what follows the last one.
fn mcp_name(name: &str) -> Option<(&str, &str)> {
    let (server, tool) = name.strip_prefix(MCP_PREFIX)?.rsplit_once("__")?;
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

/// Cleans each text piece on its own, since injected tags never span blocks,
/// and drops the empty ones. Returns the entry text and the title source: the
/// same pieces without placeholders, so a screenshot opener is titled by what
/// the user asked.
fn clean_pieces(pieces: &[Piece]) -> (String, String) {
    let cleaned: Vec<Piece> = pieces
        .iter()
        .map(|p| {
            if p.typed {
                Piece::typed(&clean_user_text(&p.text))
            } else {
                p.clone()
            }
        })
        .filter(|p| !p.text.is_empty())
        .collect();
    (join(&cleaned, false), join(&cleaned, true))
}

fn join(pieces: &[Piece], typed_only: bool) -> String {
    pieces
        .iter()
        .filter(|p| p.typed || !typed_only)
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn clean_user_text(text: &str) -> String {
    INJECTED_TAGS
        .iter()
        .fold(text.to_string(), |text, tag| remove_blocks(&text, tag))
        .trim()
        .to_string()
}

/// Removes each `<tag>…</tag>` block; an unclosed one runs to the end.
fn remove_blocks(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find(&close) {
            Some(end) => &rest[start + end + close.len()..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// `/<name>` and trimmed args, when `text` is a slash-command record. The
/// `<command-message>` and `<command-name>` tags come in either order.
fn slash_command(text: &str) -> Option<(String, String)> {
    if !(text.starts_with("<command-name>") || text.starts_with("<command-message>")) {
        return None;
    }
    let name = inner(text, "command-name")?.trim();
    let name = name.strip_prefix('/').unwrap_or(name);
    let args = inner(text, "command-args").unwrap_or_default().trim();
    Some((format!("/{name}"), args.to_string()))
}

/// The text between the first `<tag>` and the `</tag>` after it.
fn inner<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let len = text[start..].find(&close)?;
    Some(&text[start..start + len])
}

/// Removes teammate-message and task-notification blocks from `text`,
/// returning their reports in text order and the remaining text. An
/// unclosed block is left in the text.
fn agent_reports(text: &str) -> (Vec<(String, String)>, String) {
    let mut reports = Vec::new();
    let mut remaining = String::new();
    let mut rest = text;
    loop {
        let next = [TEAMMATE_OPEN, NOTIFICATION_OPEN]
            .into_iter()
            .filter_map(|open| rest.find(open).map(|i| (i, open)))
            .min();
        let Some((start, open)) = next else { break };
        let close = if open == TEAMMATE_OPEN {
            TEAMMATE_CLOSE
        } else {
            NOTIFICATION_CLOSE
        };
        let Some(len) = rest[start..].find(close) else {
            break;
        };
        let block = &rest[start..start + len];
        remaining.push_str(&rest[..start]);
        reports.extend(if open == TEAMMATE_OPEN {
            teammate_report(block)
        } else {
            notification_report(block)
        });
        rest = &rest[start + len + close.len()..];
    }
    remaining.push_str(rest);
    (reports, remaining)
}

fn teammate_report(block: &str) -> Option<(String, String)> {
    let (header, body) = split_header(block)?;
    let name = attr(header, "teammate_id")?;
    let text = body
        .lines()
        .filter(|line| !line.trim().is_empty() && !is_teammate_noise(line))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| (name.to_string(), text))
}

/// Background shells and monitors notify without a `<result>`; only agents
/// report one.
fn notification_report(block: &str) -> Option<(String, String)> {
    let text = inner(block, "result")?.trim();
    if text.is_empty() {
        return None;
    }
    let summary = inner(block, "summary").unwrap_or_default();
    let name = summary
        .split_once("Agent \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map_or("agent", |(name, _)| name);
    Some((name.to_string(), text.to_string()))
}

/// Splits at the first `>` outside a quoted attribute value.
fn split_header(block: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let end = block.find(|c| {
        if c == '"' {
            quoted = !quoted;
        }
        c == '>' && !quoted
    })?;
    Some((&block[..end], &block[end + 1..]))
}

fn attr<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = header.find(&key)? + key.len();
    let len = header[start..].find('"')?;
    Some(&header[start..start + len])
}

fn is_teammate_noise(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('{')
        && serde_json::from_str::<Value>(line)
            .is_ok_and(|v| str_at(&v, "type").is_some_and(|t| TEAMMATE_NOISE.contains(&t)))
}

fn flag(v: &Value, key: &str) -> bool {
    at(v, key).and_then(Value::as_bool) == Some(true)
}

fn non_empty<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    str_at(v, path).filter(|s| !s.is_empty())
}

fn non_blank<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    str_at(v, path).filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;
    use crate::harness::{Harness, Loaded, load};

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude")
            .join(name)
    }

    fn session(name: &str) -> Session {
        let text = fs::read_to_string(fixture_path(name)).unwrap();
        let mut s = Session::new(Harness::Claude, name.into());
        parse(&Records::parse(&text), &mut s).unwrap();
        s
    }

    fn load_fixture(name: &str) -> Loaded {
        load(&fixture_path(name), None, false).unwrap()
    }

    fn record(kind: &str, message: Value) -> Value {
        json!({
            "type": kind,
            "sessionId": "sess-1",
            "cwd": "/project",
            "version": "1.0.0",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": message,
        })
    }

    fn user(content: Value) -> Value {
        record("user", json!({"role": "user", "content": content}))
    }

    fn assistant(content: Value) -> Value {
        record(
            "assistant",
            json!({"role": "assistant", "model": "claude-3-opus", "content": content}),
        )
    }

    fn text(t: &str) -> Value {
        json!({"type": "text", "text": t})
    }

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({"type": "tool_use", "id": id, "name": name, "input": input})
    }

    fn tool_result(id: &str, content: Value) -> Value {
        json!({"type": "tool_result", "tool_use_id": id, "content": content})
    }

    fn try_parse(records: &[Value]) -> Result<Session, LoadError> {
        let jsonl = records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        parse(&Records::parse(&jsonl), &mut s).map(|()| s)
    }

    fn parse_all(records: &[Value]) -> Session {
        try_parse(records).unwrap()
    }

    /// One line per entry, for compact assertions.
    fn lines(s: &Session) -> Vec<String> {
        s.entries
            .iter()
            .map(|e| match e {
                Entry::User { text, .. } => format!("user: {text}"),
                Entry::Assistant { text, .. } => format!("assistant: {text}"),
                Entry::Thinking { text, .. } => format!("thinking: {text}"),
                Entry::Tool {
                    name,
                    summary,
                    output,
                    failed,
                    ..
                } => format!(
                    "tool: {name} [{summary}] {output:?}{}",
                    if *failed { " failed" } else { "" }
                ),
                Entry::AgentReport { name, text, .. } => format!("report {name}: {text}"),
            })
            .collect()
    }

    #[test]
    fn slash_commands_are_user_turns_with_tool_output() {
        let s = session("local_command.jsonl");
        assert_eq!(
            lines(&s),
            [
                "user: /model fable",
                r#"tool: /model [fable] Some("Set model to Fable 5.1")"#,
                "user: /doctor",
                r#"tool: /doctor [] Some("doctor: config file not found")"#,
                "user: Now explain the build error",
                "assistant: The build fails because `main` is missing a semicolon.",
            ]
        );
        assert_eq!(s.title_source.as_deref(), Some("fable"));
    }

    #[test]
    fn command_args_title_the_session() {
        let s = session("command_title.jsonl");
        assert!(!s.is_empty());
        assert_eq!(
            lines(&s),
            [
                "user: /brainstorm Design the export format",
                "assistant: What should the export contain?",
            ]
        );
        assert_eq!(s.title_source.as_deref(), Some("Design the export format"));
    }

    #[test]
    fn command_tags_in_either_order() {
        for input in [
            "<command-name>/review</command-name>\n<command-message>review</command-message>\n<command-args> src </command-args>",
            "<command-message>review</command-message>\n<command-name>review</command-name>\n<command-args>src</command-args>",
        ] {
            assert_eq!(
                lines(&parse_all(&[user(json!(input))])),
                ["user: /review src"],
                "{input}"
            );
        }
    }

    #[test]
    fn command_record_is_the_command() {
        let s = parse_all(&[user(json!(
            "<command-name>foo</command-name>actual message"
        ))]);
        assert_eq!(lines(&s), ["user: /foo"]);
    }

    #[test]
    fn command_tags_count_only_at_the_start() {
        let s = parse_all(&[
            user(json!("<command-args>some args</command-args>")),
            user(json!(
                "<command-message>foo</command-message>actual message"
            )),
            user(json!("<command-name>unclosed")),
            user(json!("How do I parse <command-name> tags?")),
        ]);
        assert_eq!(
            lines(&s),
            [
                "user: <command-args>some args</command-args>",
                "user: actual message",
                "user: <command-name>unclosed",
                "user: How do I parse <command-name> tags?",
            ]
        );
    }

    #[test]
    fn command_output_without_a_command() {
        let s = parse_all(&[
            user(json!(
                "<local-command-stdout>output here</local-command-stdout>"
            )),
            user(json!(
                "<local-command-stderr>error output</local-command-stderr>"
            )),
        ]);
        assert_eq!(
            lines(&s),
            [
                r#"tool: local command [] Some("output here")"#,
                r#"tool: local command [] Some("error output")"#,
            ]
        );
    }

    #[test]
    fn stdout_then_stderr() {
        let s = parse_all(&[
            user(json!(
                "<command-name>/doctor</command-name><command-args></command-args>"
            )),
            user(json!(
                "<local-command-stdout> ok </local-command-stdout><local-command-stderr>warn</local-command-stderr>"
            )),
        ]);
        assert_eq!(lines(&s)[1], r#"tool: /doctor [] Some("ok\nwarn")"#);
    }

    #[test]
    fn shell_mode_input_and_output() {
        let s = parse_all(&[
            user(json!("<bash-input>ls -a</bash-input>")),
            user(json!(
                "<bash-stdout>Cargo.toml</bash-stdout><bash-stderr></bash-stderr>"
            )),
            user(json!("<bash-input>false</bash-input>")),
            user(json!(
                "<bash-stdout></bash-stdout><bash-stderr>  </bash-stderr>"
            )),
        ]);
        assert_eq!(
            lines(&s),
            [
                "user: ! ls -a",
                r#"tool: ! [ls -a] Some("Cargo.toml")"#,
                "user: ! false",
                "tool: ! [false] None",
            ]
        );
    }

    #[test]
    fn conversation_forgets_the_last_command() {
        let s = parse_all(&[
            user(json!(
                "<command-name>/model</command-name><command-args>fable</command-args>"
            )),
            user(json!("Thanks")),
            user(json!("<local-command-stdout>late</local-command-stdout>")),
        ]);
        assert_eq!(lines(&s)[2], r#"tool: local command [] Some("late")"#);
    }

    #[test]
    fn teammate_message_is_a_report() {
        let s = parse_all(&[user(json!(
            "<teammate-message teammate_id=\"researcher\" summary=\"Found the issue\">\nI found the bug in line 42 of app.ts.\nThe variable is undefined.\n</teammate-message>"
        ))]);
        assert_eq!(
            lines(&s),
            [
                "report researcher: I found the bug in line 42 of app.ts.\nThe variable is undefined."
            ]
        );
    }

    #[test]
    fn teammate_noise_is_filtered() {
        for (body, want) in [
            (
                "Working on the task.\n{\"type\":\"idle_notification\",\"idle_since\":\"2025-01-01\"}\nAlmost done.",
                vec!["report worker: Working on the task.\nAlmost done."],
            ),
            (
                "{\"type\":\"shutdown_request\",\"requestId\":\"abc-123\"}\n{\"type\":\"shutdown_approved\",\"requestId\":\"abc-123\"}\n{\"type\":\"teammate_terminated\",\"teammate_id\":\"helper\"}\nGoodbye!",
                vec!["report worker: Goodbye!"],
            ),
            (
                "{\"type\":\"idle_notification\",\"idle_since\":\"2025-01-01\"}\n{\"type\":\"shutdown_approved\",\"requestId\":\"abc-123\"}",
                vec![],
            ),
        ] {
            let message = format!(
                "<teammate-message teammate_id=\"worker\" summary=\"Status\">\n{body}\n</teammate-message>"
            );
            assert_eq!(lines(&parse_all(&[user(json!(message))])), want, "{body}");
        }
    }

    #[test]
    fn quoted_gt_does_not_end_teammate_header() {
        let s = parse_all(&[user(json!(
            "<teammate-message teammate_id=\"a\" summary=\"a -> b\">\nreport\n</teammate-message>"
        ))]);
        assert_eq!(lines(&s), ["report a: report"]);
    }

    #[test]
    fn prose_mentioning_a_tag_name_is_user_text() {
        let s = parse_all(&[user(json!("Fix the bug in teammate-message handler"))]);
        assert_eq!(lines(&s), ["user: Fix the bug in teammate-message handler"]);
    }

    #[test]
    fn agent_reports_fixture() {
        let s = session("agent_reports.jsonl");
        assert_eq!(
            lines(&s),
            [
                "user: Survey the parser module in the background",
                r#"tool: Agent [Survey parser] Some("Async agent launched successfully.\nagentId: a1b2c3d4e5f6a7b8c")"#,
                "report Survey parser: parser.rs exposes `parse` and `Record`.",
                "report researcher: The bug is in the tokenizer.",
                "user: Thanks, please fix it",
                "assistant: Fixing the tokenizer.",
            ]
        );
        assert!(s.unhandled.is_empty(), "{:?}", s.unhandled);
    }

    #[test]
    fn reports_keep_text_order() {
        let message = "<task-notification>\n<summary>Monitor event: \"CI\"</summary>\n<event>done</event>\n</task-notification>\n\
            <task-notification>\n<summary>Agent \"Late\" completed</summary>\n<result>late result</result>\n</task-notification>\n\
            <teammate-message teammate_id=\"a\">\nfrom a\n</teammate-message>\n\
            <task-notification>\n<summary>no agent name</summary>\n<result>anon</result>\n</task-notification>\n\
            <teammate-message missing=\"id\">\nx\n</teammate-message>\ntrailing words";
        let s = parse_all(&[user(json!(message))]);
        assert_eq!(
            lines(&s),
            [
                "report Late: late result",
                "report a: from a",
                "report agent: anon",
                "user: trailing words",
            ]
        );
    }

    #[test]
    fn title_source_skips_agent_reports() {
        let s = parse_all(&[user(json!([
            {"type": "image", "source": {}},
            text("<teammate-message teammate_id=\"a\">\nreport\n</teammate-message>Please look"),
        ]))]);
        assert_eq!(
            lines(&s),
            ["report a: report", "user: [image]\n\nPlease look"]
        );
        assert_eq!(s.title_source.as_deref(), Some("Please look"));
    }

    #[test]
    fn unclosed_report_stays_as_text() {
        let s = parse_all(&[user(json!(
            "<teammate-message teammate_id=\"a\">\nnever closed"
        ))]);
        assert_eq!(
            lines(&s),
            ["user: <teammate-message teammate_id=\"a\">\nnever closed"]
        );
    }

    #[test]
    fn metadata_basic_fields() {
        let s = parse_all(&[user(json!("hello"))]);
        assert_eq!(s.id, "sess-1");
        assert_eq!(s.cwds, [PathBuf::from("/project")]);
        assert_eq!(s.harness_version.as_deref(), Some("1.0.0"));
        assert_eq!(s.cc_version.as_deref(), Some("v1.0.0"));
        assert_eq!(s.started_at.unwrap().to_string(), "2025-01-01T00:00:00Z");
    }

    #[test]
    fn model_from_first_real_assistant() {
        let mut synthetic = assistant(json!([text("Spend limit reached")]));
        synthetic["message"]["model"] = json!("<synthetic>");
        let s = parse_all(&[
            user(json!("hi")),
            synthetic,
            assistant(json!([text("hello")])),
        ]);
        assert_eq!(s.model.as_deref(), Some("claude-3-opus"));
        assert_eq!(
            lines(&s),
            [
                "user: hi",
                "assistant: Spend limit reached",
                "assistant: hello"
            ]
        );
    }

    // None, not "unknown", when there is no assistant record.
    #[test]
    fn model_absent_without_assistant() {
        assert_eq!(parse_all(&[user(json!("hi"))]).model, None);
    }

    #[test]
    fn metadata_takes_first_non_empty_values() {
        let mut first = user(json!("a"));
        first["sessionId"] = json!("first");
        first["cwd"] = json!("");
        let mut second = user(json!("b"));
        second["sessionId"] = json!("second");
        second["cwd"] = json!("/real");
        let s = parse_all(&[first, second]);
        assert_eq!(s.id, "first");
        assert_eq!(s.cwds, [PathBuf::from("/real")]);
    }

    #[test]
    fn unparseable_timestamp_is_skipped() {
        let mut bad = user(json!("a"));
        bad["timestamp"] = json!("t");
        let s = parse_all(&[bad, user(json!("b"))]);
        assert_eq!(s.started_at.unwrap().to_string(), "2025-01-01T00:00:00Z");
        assert!(matches!(s.entries[0], Entry::User { at: None, .. }));
    }

    #[test]
    fn missing_session_id_is_a_parse_error() {
        let err = try_parse(&[json!({"type": "user", "message": {"content": "hi"}})]).unwrap_err();
        assert!(
            matches!(&err, LoadError::Parse(m) if m == "missing sessionId"),
            "{err:?}"
        );
    }

    #[test]
    fn sidechain_only_file_loads_as_empty() {
        let mut side_user = user(json!("hidden"));
        side_user["isSidechain"] = json!(true);
        let mut side_assistant = assistant(json!([text("also hidden")]));
        side_assistant["isSidechain"] = json!(true);

        let s = try_parse(&[side_user.clone(), side_assistant.clone()]).unwrap();
        assert!(s.entries.is_empty());

        let jsonl = [side_user, side_assistant]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(jsonl.as_bytes()).unwrap();
        assert!(matches!(load(f.path(), None, false), Ok(Loaded::Empty)));
    }

    #[test]
    fn user_string_content() {
        assert_eq!(
            lines(&parse_all(&[user(json!("Hello there"))])),
            ["user: Hello there"]
        );
    }

    #[test]
    fn blank_user_content_has_no_entry() {
        assert!(
            parse_all(&[user(json!("")), user(json!("   "))])
                .entries
                .is_empty()
        );
    }

    #[test]
    fn assistant_text_and_thinking() {
        let s = parse_all(&[assistant(json!([
            {"type": "thinking", "thinking": "Let me think...", "signature": "c2ln"},
            text("Answer"),
        ]))]);
        assert_eq!(
            lines(&s),
            ["thinking: Let me think...", "assistant: Answer"]
        );
    }

    #[test]
    fn blank_text_and_thinking_are_dropped() {
        let s = parse_all(&[
            assistant(json!([
                text("   "),
                {"type": "thinking", "thinking": "  "},
                {"type": "redacted_thinking", "data": "ZW5j"},
                text("real"),
            ])),
            assistant(json!([text("   ")])),
        ]);
        assert_eq!(lines(&s), ["assistant: real"]);
        assert!(s.unhandled.is_empty());
    }

    // No merging: the renderer groups runs.
    #[test]
    fn consecutive_assistant_records_stay_separate() {
        let s = parse_all(&[
            assistant(json!([text("Part 1")])),
            assistant(json!([text("Part 2")])),
        ]);
        assert_eq!(lines(&s), ["assistant: Part 1", "assistant: Part 2"]);
    }

    #[test]
    fn user_turn_separates_assistant_records() {
        let s = parse_all(&[
            assistant(json!([text("A1")])),
            user(json!("User msg")),
            assistant(json!([text("A2")])),
        ]);
        assert_eq!(
            lines(&s),
            ["assistant: A1", "user: User msg", "assistant: A2"]
        );
    }

    #[test]
    fn meta_and_sidechain_records_are_skipped() {
        let mut meta = user(json!("hidden"));
        meta["isMeta"] = json!(true);
        let mut side = assistant(json!([text("side")]));
        side["isSidechain"] = json!(true);
        side["cwd"] = json!("/elsewhere");
        let s = parse_all(&[meta, side, user(json!("shown"))]);
        assert_eq!(lines(&s), ["user: shown"]);
        assert_eq!(s.cwds, [PathBuf::from("/project")]);
    }

    #[test]
    fn ignored_records_are_silent() {
        let s = parse_all(&[
            user(json!("hello")),
            record("progress", json!({})),
            record("system", json!({})),
            record("file-history-snapshot", json!({})),
            assistant(json!([])),
        ]);
        assert_eq!(lines(&s), ["user: hello"]);
        assert!(s.unhandled.is_empty(), "{:?}", s.unhandled);
    }

    #[test]
    fn injected_tags_are_stripped() {
        for (input, want) in [
            (
                "<system-reminder>secret stuff</system-reminder>actual message",
                "actual message",
            ),
            (
                "<local-command-caveat>hidden</local-command-caveat>visible",
                "visible",
            ),
            (
                "<command-message>foo</command-message>actual message",
                "actual message",
            ),
            ("before <system-reminder>unclosed", "before"),
        ] {
            assert_eq!(
                lines(&parse_all(&[user(json!(input))])),
                [format!("user: {want}")],
                "{input}"
            );
        }
    }

    #[test]
    fn bash_notification_is_dropped() {
        let s = parse_all(&[user(json!(
            "<bash-notification>\n<shell-id>b1</shell-id>\n<status>failed</status>\n<summary>Background command failed</summary>\n</bash-notification>\nnext step"
        ))]);
        assert_eq!(lines(&s), ["user: next step"]);
    }

    #[test]
    fn reminder_only_message_has_no_entry() {
        let s = parse_all(&[user(json!("<system-reminder>only tags</system-reminder>"))]);
        assert!(s.entries.is_empty());
        assert!(s.is_empty());
    }

    #[test]
    fn compaction_summary_is_an_agent_report() {
        let mut compact = user(json!(
            "This session is being continued.\n\nSummary: work so far."
        ));
        compact["isCompactSummary"] = json!(true);
        let s = parse_all(&[user(json!("hi")), compact]);
        assert_eq!(
            lines(&s),
            [
                "user: hi",
                "report compaction: This session is being continued.\n\nSummary: work so far.",
            ]
        );
    }

    #[test]
    fn attachments_are_marked() {
        let s = session("attachments.jsonl");
        assert_eq!(
            lines(&s),
            [
                "user: [image]\n\nWhat is wrong in this screenshot?",
                "user: [document]\n\n[document: spec.pdf]\n\nCompare with these",
                "assistant: The button overlaps the header.",
            ]
        );
    }

    #[test]
    fn title_source_is_first_user_text() {
        let s = session("basic.jsonl");
        assert_eq!(
            s.title_source.as_deref(),
            Some("Add a greeting to the sample CLI")
        );
    }

    #[test]
    fn title_source_skips_placeholders() {
        let s = session("attachments.jsonl");
        assert_eq!(
            s.title_source.as_deref(),
            Some("What is wrong in this screenshot?")
        );
    }

    #[test]
    fn title_source_rules() {
        let image = json!({"type": "image", "source": {}});
        let titled_doc = json!({"type": "document", "title": "spec.pdf", "source": {}});
        for (content, want) in [
            (json!([image.clone()]), "[image]"),
            (
                json!([titled_doc, image.clone()]),
                "[document: spec.pdf]\n\n[image]",
            ),
            (
                json!([image.clone(), text("See [image] below")]),
                "See [image] below",
            ),
            (
                json!([text("Line one\n\nLine two"), image.clone()]),
                "Line one\n\nLine two",
            ),
            (
                json!([image, text("<system-reminder>injected</system-reminder>")]),
                "[image]",
            ),
        ] {
            let s = parse_all(&[user(content.clone()), user(json!("second"))]);
            assert_eq!(s.title_source.as_deref(), Some(want), "{content}");
        }
    }

    #[test]
    fn pieces_are_cleaned_one_by_one() {
        let s = parse_all(&[user(json!([
            text("a"),
            text("<system-reminder>injected</system-reminder>"),
            {"type": "image", "source": {}},
            text("b"),
        ]))]);
        assert_eq!(lines(&s), ["user: a\n\n[image]\n\nb"]);
        assert_eq!(s.title_source.as_deref(), Some("a\n\nb"));
    }

    #[test]
    fn unknown_records_and_blocks_are_tallied() {
        let s = session("unknown.jsonl");
        assert_eq!(
            s.unhandled,
            BTreeMap::from([
                ("claude:block/server_tool_use".to_string(), 1),
                ("claude:record/hologram".to_string(), 1),
            ])
        );
        assert_eq!(lines(&s), ["user: Hello", "assistant: Hi."]);
    }

    #[test]
    fn malformed_mid_file_line_is_tallied() {
        let Loaded::Session(s) = load_fixture("malformed.jsonl") else {
            panic!("expected a session");
        };
        assert_eq!(
            s.unhandled,
            BTreeMap::from([("claude:malformed-line".to_string(), 1)])
        );
        assert_eq!(
            lines(&s),
            [
                "user: Summarise the log",
                "assistant: The log shows two warnings."
            ]
        );
    }

    #[test]
    fn snapshot_only_is_empty() {
        assert!(matches!(load_fixture("snapshot_only.jsonl"), Loaded::Empty));
    }

    #[test]
    fn mistyped_fields_degrade() {
        let s = parse_all(&[
            user(json!(42)),
            record("user", json!("not an object")),
            json!({"type": "assistant", "sessionId": "s", "message": {"content": "a string"}}),
            user(json!([{"type": "text", "text": 7}, "bare string", {"no": "type"}])),
        ]);
        assert!(s.entries.is_empty());
        assert_eq!(
            s.unhandled,
            BTreeMap::from([("claude:block/".to_string(), 2)])
        );
    }

    #[test]
    fn tool_use_pairs_with_result() {
        let s = parse_all(&[
            assistant(json!([tool_use(
                "tool-1",
                "Bash",
                json!({"command": "ls"})
            )])),
            user(json!([tool_result("tool-1", json!("file.txt"))])),
        ]);
        assert_eq!(lines(&s), [r#"tool: Bash [ls] Some("file.txt")"#]);
    }

    #[test]
    fn result_blocks_join_with_newlines() {
        let s = parse_all(&[
            assistant(json!([tool_use("tool-1", "Read", json!({}))])),
            user(json!([tool_result(
                "tool-1",
                json!([
                    text("line1"),
                    text("line2"),
                    {"type": "tool_reference", "tool_name": "Read"},
                    {"type": "image", "source": {}},
                    {"type": "hologram"},
                ])
            )])),
        ]);
        assert_eq!(
            lines(&s),
            [r#"tool: Read [] Some("line1\nline2\n[image]")"#]
        );
        assert_eq!(
            s.unhandled,
            BTreeMap::from([("claude:block/hologram".to_string(), 1)])
        );
    }

    #[test]
    fn text_block_without_text_field_is_skipped() {
        let s = parse_all(&[
            assistant(json!([tool_use("tool-1", "Read", json!({}))])),
            user(json!([tool_result(
                "tool-1",
                json!([text("a"), {"type": "text"}, text("b")])
            )])),
        ]);
        assert_eq!(lines(&s), [r#"tool: Read [] Some("a\nb")"#]);
    }

    // None, not an empty string, when the result has no content.
    #[test]
    fn empty_result_is_none() {
        let s = parse_all(&[
            assistant(json!([tool_use("tool-1", "Bash", json!({}))])),
            user(json!([{"type": "tool_result", "tool_use_id": "tool-1"}])),
            assistant(json!([tool_use("tool-2", "Bash", json!({}))])),
            user(json!([tool_result("tool-2", json!("  \n"))])),
        ]);
        assert_eq!(lines(&s), ["tool: Bash [] None", "tool: Bash [] None"]);
    }

    #[test]
    fn is_error_sets_failed() {
        let mut result = tool_result("tool-1", json!("error msg"));
        result["is_error"] = json!(true);
        let s = parse_all(&[
            assistant(json!([tool_use("tool-1", "Bash", json!({}))])),
            user(json!([result])),
        ]);
        assert_eq!(lines(&s), [r#"tool: Bash [] Some("error msg") failed"#]);
    }

    #[test]
    fn user_text_alongside_results() {
        let s = parse_all(&[
            assistant(json!([tool_use("tool-1", "Bash", json!({}))])),
            user(json!([
                tool_result("tool-1", json!("ok")),
                text("User followup")
            ])),
        ]);
        assert_eq!(
            lines(&s),
            [r#"tool: Bash [] Some("ok")"#, "user: User followup"]
        );
    }

    #[test]
    fn realistic_conversation_keeps_file_order() {
        let s = parse_all(&[
            user(json!("Fix the bug")),
            assistant(json!([
                {"type": "thinking", "thinking": "I need to read the file"},
                text("Let me look at the code."),
                tool_use("t1", "Read", json!({"file_path": "src/app.ts"})),
            ])),
            user(json!([tool_result("t1", json!("const x = 1;"))])),
            assistant(json!([
                text("I found the issue."),
                tool_use("t2", "Edit", json!({"file_path": "src/app.ts"})),
            ])),
            user(json!([tool_result("t2", json!("ok"))])),
            assistant(json!([text("Done!")])),
        ]);
        assert_eq!(
            lines(&s),
            [
                "user: Fix the bug",
                "thinking: I need to read the file",
                "assistant: Let me look at the code.",
                r#"tool: Read [src/app.ts] Some("const x = 1;")"#,
                "assistant: I found the issue.",
                r#"tool: Edit [src/app.ts] Some("ok")"#,
                "assistant: Done!",
            ]
        );
    }

    #[test]
    fn unmatched_results_are_dropped() {
        let s = parse_all(&[
            user(json!([tool_result("early", json!("too soon"))])),
            assistant(json!([
                tool_use("early", "Read", json!({})),
                tool_use("once", "Read", json!({})),
            ])),
            user(json!([
                tool_result("once", json!("first")),
                tool_result("once", json!("second")),
                tool_result("nobody", json!("orphan")),
            ])),
        ]);
        assert_eq!(
            lines(&s),
            ["tool: Read [] None", r#"tool: Read [] Some("first")"#]
        );
    }

    #[test]
    fn mistyped_tool_fields_degrade() {
        let s = parse_all(&[
            assistant(json!([
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": "ls"},
                {"type": "tool_use"},
            ])),
            user(json!([{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": {"text": "x"},
                "is_error": "yes",
            }])),
        ]);
        assert_eq!(lines(&s), ["tool: Bash [] None", "tool:  [] None"]);
    }

    #[test]
    fn basic_fixture() {
        let s = session("basic.jsonl");
        assert_eq!(
            lines(&s),
            [
                "user: Add a greeting to the sample CLI",
                "thinking: The user wants a greeting. Look at main.rs first.",
                r#"tool: Read [/Users/user/Workspaces/sample/src/main.rs] Some("1\tfn main() {}\n")"#,
                r#"tool: Bash [cargo test] Some("error: could not compile `sample`") failed"#,
                r#"tool: Edit [/Users/user/Worktrees/sample/feature/src/main.rs] Some("The file has been updated.")"#,
                "assistant: Added a `println!` greeting. Tests failed to compile before the edit.",
            ]
        );
        assert_eq!(
            s.cwds,
            [
                PathBuf::from("/Users/user/Workspaces/sample"),
                PathBuf::from("/Users/user/Worktrees/sample/feature"),
            ]
        );
        assert_eq!(s.cc_version.as_deref(), Some("v2.1.259"));
        assert_eq!(s.model.as_deref(), Some("claude-fable-5-1"));
        assert!(s.unhandled.is_empty(), "{:?}", s.unhandled);
    }

    #[test]
    fn tool_edges_fixture() {
        let s = session("tool_edges.jsonl");
        assert_eq!(
            lines(&s),
            [
                "user: Search the docs for the config format",
                r#"tool: Grep [config] Some("docs/config.md\n[image]")"#,
                "assistant: Reading the file now.",
                "tool: Read [/Users/user/Workspaces/sample/docs/config.md] None",
            ]
        );
        assert!(s.unhandled.is_empty(), "{:?}", s.unhandled);
    }

    #[test]
    fn summaries_by_tool() {
        for (name, input, want) in [
            ("Bash", json!({"command": "cargo test"}), "cargo test"),
            ("Read", json!({"file_path": "/a"}), "/a"),
            ("Write", json!({"file_path": "/b"}), "/b"),
            ("Edit", json!({"file_path": "/c"}), "/c"),
            (
                "NotebookEdit",
                json!({"notebook_path": "/n.ipynb"}),
                "/n.ipynb",
            ),
            ("Glob", json!({"pattern": "**/*.rs"}), "**/*.rs"),
            ("Grep", json!({"pattern": "fn main"}), "fn main"),
            ("WebSearch", json!({"query": "rust"}), "rust"),
            ("ToolSearch", json!({"query": "select:Read"}), "select:Read"),
            (
                "WebFetch",
                json!({"url": "https://example.com"}),
                "https://example.com",
            ),
            (
                "Agent",
                json!({"description": "Survey", "prompt": "long"}),
                "Survey",
            ),
            ("Task", json!({"description": "Old survey"}), "Old survey"),
            ("Skill", json!({"skill": "review"}), "review"),
            ("TaskCreate", json!({"subject": "Write docs"}), "Write docs"),
            ("TaskUpdate", json!({"taskId": "7"}), "7"),
            ("SendMessage", json!({"to": "researcher"}), "researcher"),
            (
                "AskUserQuestion",
                json!({"questions": [{"question": "Which?"}, {"question": "Second?"}]}),
                "Which?",
            ),
            ("Bash", json!({"command": 5}), ""),
            ("AskUserQuestion", json!({"questions": []}), ""),
            ("Unlisted", json!({"anything": "x"}), ""),
        ] {
            assert_eq!(
                tool_label(name, &input),
                (name.to_string(), want.to_string()),
                "{name}"
            );
        }
    }

    #[test]
    fn mcp_tools_are_named_by_server() {
        let input = json!({"query": "parser", "limit": 5});
        assert_eq!(
            tool_label("mcp__github__search_code", &input),
            (
                "github.search_code".to_string(),
                r#"{"query":"parser","limit":5}"#.to_string()
            )
        );
        assert_eq!(
            tool_label("mcp__plugin_x__server__search", &input).0,
            "plugin_x__server.search"
        );
        assert_eq!(
            tool_label("mcp__foo", &input),
            ("mcp__foo".to_string(), String::new())
        );
        assert_eq!(
            tool_label("mcp__github__ping", &Value::Null),
            ("github.ping".to_string(), String::new())
        );
    }

    #[test]
    fn modern_fixture() {
        let s = session("modern.jsonl");
        assert_eq!(s.model.as_deref(), Some("claude-fable-5-1"));
        assert_eq!(
            lines(&s),
            [
                "user: Check the repo",
                "assistant: You've hit your monthly spend limit",
                "user: ! ls",
                r#"tool: ! [ls] Some("Cargo.toml")"#,
                "user: What does it contain?",
                r#"tool: local command [] Some("stray output")"#,
                r#"tool: Agent [Survey tests] Some("Three tests.")"#,
                r#"tool: Skill [review] Some("Launching skill: review")"#,
                r#"tool: AskUserQuestion [Which module first?] Some("User answered: parser")"#,
                r#"tool: plugin_x__server.search [{"query":"parser","limit":5}] Some("parser.rs")"#,
                "report compaction: This session is being continued from a previous conversation that ran out of context.\n\nSummary:\n1. The user asked to check the repo.",
                "assistant: Continuing with the parser.",
            ]
        );
        assert!(s.unhandled.is_empty(), "{:?}", s.unhandled);
    }

    #[test]
    fn known_fixtures_tally_nothing() {
        for name in [
            "basic.jsonl",
            "tool_edges.jsonl",
            "local_command.jsonl",
            "command_title.jsonl",
            "agent_reports.jsonl",
            "attachments.jsonl",
            "modern.jsonl",
        ] {
            let s = session(name);
            assert!(s.unhandled.is_empty(), "{name}: {:?}", s.unhandled);
        }
    }

    #[test]
    fn basic_snapshot() {
        insta::assert_debug_snapshot!("basic", session("basic.jsonl"));
    }

    #[test]
    fn modern_snapshot() {
        insta::assert_debug_snapshot!("modern", session("modern.jsonl"));
    }
}
