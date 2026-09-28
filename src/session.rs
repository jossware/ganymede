use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::Timestamp;

use crate::harness::Harness;

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub harness: Harness,
    pub id: String,
    pub harness_version: Option<String>,
    pub model: Option<String>,
    /// Distinct working directories in first-seen order.
    pub cwds: Vec<PathBuf>,
    pub started_at: Option<Timestamp>,
    /// Adapter-cleaned text the title and slug derive from.
    pub title_source: Option<String>,
    /// Claude only, kept for frontmatter compatibility with cc2md.
    pub cc_version: Option<String>,
    pub entries: Vec<Entry>,
    /// Counts keyed `<harness>:<kind>/<type>` or `<harness>:malformed-line`.
    pub unhandled: BTreeMap<String, usize>,
    pub transcript: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User {
        at: Option<Timestamp>,
        text: String,
    },
    Assistant {
        at: Option<Timestamp>,
        text: String,
    },
    Thinking {
        at: Option<Timestamp>,
        text: String,
    },
    Tool {
        at: Option<Timestamp>,
        name: String,
        summary: String,
        output: Option<String>,
        /// The harness's own failure signal, never inferred from output.
        failed: bool,
    },
    AgentReport {
        at: Option<Timestamp>,
        name: String,
        text: String,
    },
}

impl Entry {
    pub fn at(&self) -> Option<Timestamp> {
        match self {
            Entry::User { at, .. }
            | Entry::Assistant { at, .. }
            | Entry::Thinking { at, .. }
            | Entry::Tool { at, .. }
            | Entry::AgentReport { at, .. } => *at,
        }
    }
}

impl Session {
    pub fn new(harness: Harness, transcript: PathBuf) -> Self {
        Self {
            harness,
            id: String::new(),
            harness_version: None,
            model: None,
            cwds: Vec::new(),
            started_at: None,
            title_source: None,
            cc_version: None,
            entries: Vec::new(),
            unhandled: BTreeMap::new(),
            transcript,
        }
    }

    /// A session without a user turn produces no note.
    pub fn is_empty(&self) -> bool {
        !self.entries.iter().any(|e| matches!(e, Entry::User { .. }))
    }

    pub fn add_cwd(&mut self, cwd: PathBuf) {
        if !self.cwds.contains(&cwd) {
            self.cwds.push(cwd);
        }
    }

    /// Counts unknown content. `what` is `<kind>/<type>` or `malformed-line`.
    pub fn record_unhandled(&mut self, what: &str, count: usize) {
        if count == 0 {
            return;
        }
        let key = format!("{}:{what}", self.harness.id());
        *self.unhandled.entry(key).or_default() += count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Entry {
        Entry::User {
            at: None,
            text: text.into(),
        }
    }

    #[test]
    fn empty_without_user_entry() {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        assert!(s.is_empty());
        s.entries.push(Entry::Assistant {
            at: None,
            text: "hi".into(),
        });
        assert!(s.is_empty());
        s.entries.push(user("hello"));
        assert!(!s.is_empty());
    }

    #[test]
    fn cwds_keep_first_seen_order() {
        let mut s = Session::new(Harness::Codex, "t.jsonl".into());
        for cwd in ["/a", "/b", "/a", "/c", "/b"] {
            s.add_cwd(cwd.into());
        }
        assert_eq!(s.cwds, [PathBuf::from("/a"), "/b".into(), "/c".into()]);
    }

    #[test]
    fn unhandled_keys_carry_harness() {
        let mut s = Session::new(Harness::Codex, "t.jsonl".into());
        s.record_unhandled("item/Foo", 1);
        s.record_unhandled("item/Foo", 2);
        s.record_unhandled("malformed-line", 1);
        s.record_unhandled("event/bar", 0);
        assert_eq!(
            s.unhandled,
            BTreeMap::from([
                ("codex:item/Foo".to_string(), 3),
                ("codex:malformed-line".to_string(), 1),
            ])
        );
    }
}
