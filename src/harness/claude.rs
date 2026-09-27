use jiff::Timestamp;
use serde_json::Value;

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
const INJECTED_TAGS: &[&str] = &["system-reminder", "local-command-caveat", "command-message"];

pub fn is_ignored_record(record: &Value) -> bool {
    record
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| IGNORED_RECORDS.contains(&t))
}

pub fn parse(records: &Records, session: &mut Session) -> Result<(), LoadError> {
    let mut walk = Walk::default();
    for record in &records.values {
        // Subagent output reaches the main thread through tool results and
        // task notifications.
        if flag(record, "isSidechain") {
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
    if session.id.is_empty() {
        return Err(LoadError::Parse("missing sessionId".into()));
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
struct Walk {}

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

    fn user_text(&mut self, pieces: &[Piece], ts: Option<Timestamp>, session: &mut Session) {
        let (text, title) = clean_pieces(pieces);
        if !text.is_empty() {
            push_user(session, ts, text, &title);
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
                t => session.record_unhandled(&format!("block/{t}"), 1),
            }
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
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;
    use crate::harness::{Harness, Loaded, load};
    use crate::session::Entry;

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

    // cc2md: ExtractMetadata_BasicFields
    #[test]
    fn metadata_basic_fields() {
        let s = parse_all(&[user(json!("hello"))]);
        assert_eq!(s.id, "sess-1");
        assert_eq!(s.cwds, [PathBuf::from("/project")]);
        assert_eq!(s.harness_version.as_deref(), Some("1.0.0"));
        assert_eq!(s.cc_version.as_deref(), Some("v1.0.0"));
        assert_eq!(s.started_at.unwrap().to_string(), "2025-01-01T00:00:00Z");
    }

    // cc2md: ExtractMetadata_ModelFromAssistant, plus the <synthetic> decision.
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

    // cc2md: ExtractMetadata_DefaultModel. Changed: None, not "unknown".
    #[test]
    fn model_absent_without_assistant() {
        assert_eq!(parse_all(&[user(json!("hi"))]).model, None);
    }

    // cc2md: ExtractMetadata_FirstNonEmptyValues
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

    // cc2md: BuildTurns_UserStringContent
    #[test]
    fn user_string_content() {
        assert_eq!(
            lines(&parse_all(&[user(json!("Hello there"))])),
            ["user: Hello there"]
        );
    }

    // cc2md: BuildTurns_SkipEmptyString, FormatUserTurn_EmptyForWhitespaceOnly
    #[test]
    fn blank_user_content_has_no_entry() {
        assert!(
            parse_all(&[user(json!("")), user(json!("   "))])
                .entries
                .is_empty()
        );
    }

    // cc2md: BuildTurns_AssistantTextBlocks, BuildTurns_ThinkingBlocks
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

    // cc2md: BuildTurns_SkipEmptyTextAndThinking, BuildTurns_SkipEmptyAssistant
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

    // cc2md: BuildTurns_MergeConsecutiveAssistant. Changed: no merging; the
    // renderer groups runs.
    #[test]
    fn consecutive_assistant_records_stay_separate() {
        let s = parse_all(&[
            assistant(json!([text("Part 1")])),
            assistant(json!([text("Part 2")])),
        ]);
        assert_eq!(lines(&s), ["assistant: Part 1", "assistant: Part 2"]);
    }

    // cc2md: BuildTurns_NoMergeWhenSeparatedByUser
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

    // cc2md: ReadSessionFile_SkipsMetaLines, plus sidechains.
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

    // cc2md: ReadSessionFile_ValidLines, ReadSessionFile_SkipsFilteredTypes
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

    // cc2md: FormatUserTurn_StripSystemReminder, _StripLocalCommandCaveat,
    // _StripCommandMessage
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
            assert_eq!(clean_user_text(input), want, "{input}");
        }
    }

    // cc2md: FormatUserTurn_EmptyWhenOnlyTags
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
}
