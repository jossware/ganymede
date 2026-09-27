use serde_json::Value;

use super::{LoadError, Records};
use crate::session::Session;

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

pub fn is_ignored_record(record: &Value) -> bool {
    record
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| IGNORED_RECORDS.contains(&t))
}

pub fn parse(_records: &Records, _session: &mut Session) -> Result<(), LoadError> {
    Err(LoadError::Parse("claude adapter not implemented".into()))
}
