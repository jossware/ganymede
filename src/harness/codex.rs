use serde_json::Value;

use super::json::{at, str_at};
use super::{LoadError, Records};
use crate::session::Session;

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

pub fn parse(_records: &Records, _session: &mut Session) -> Result<(), LoadError> {
    Err(LoadError::Parse("codex adapter not implemented".into()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(name)
    }

    fn fixture(name: &str) -> Records {
        Records::parse(&fs::read_to_string(fixture_path(name)).unwrap())
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
}
