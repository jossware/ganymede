//! The note body: entries laid out in file order.

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::Options;
use super::escape::{self, Block};
use crate::session::{Entry, Session};

pub fn body(session: &Session, opts: &Options) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut run: Vec<&Entry> = Vec::new();
    for entry in session.entries.iter().filter(|e| keep(e, opts)) {
        match entry {
            Entry::User { .. } | Entry::AgentReport { .. } => {
                if !run.is_empty() {
                    blocks.push(run_block(&run, session, opts));
                    run.clear();
                }
                blocks.push(callout(entry, opts));
            }
            _ => run.push(entry),
        }
    }
    if !run.is_empty() {
        blocks.push(run_block(&run, session, opts));
    }
    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

fn keep(entry: &Entry, opts: &Options) -> bool {
    match entry {
        Entry::Thinking { text, .. } => opts.thinking && !text.trim().is_empty(),
        Entry::Assistant { text, .. } => !text.trim().is_empty(),
        _ => true,
    }
}

fn run_block(run: &[&Entry], session: &Session, opts: &Options) -> String {
    let header = format!(
        "> **{}**{}",
        session.harness.display(),
        label(run[0].at(), &opts.tz)
    );
    let mut parts = vec![header];
    parts.extend(run.iter().map(|e| run_entry(e, opts)));
    parts.join("\n\n")
}

fn run_entry(entry: &Entry, opts: &Options) -> String {
    match entry {
        Entry::Assistant { text, .. } => prose(text),
        Entry::Thinking { text, .. } => format!("**Thinking**\n\n{}", prose(text)),
        Entry::Tool {
            name,
            summary,
            output,
            failed,
            ..
        } => tool(name, summary, output.as_deref(), *failed, opts),
        Entry::User { .. } | Entry::AgentReport { .. } => {
            unreachable!("callouts never join a run")
        }
    }
}

/// Separators supply every blank line, so an entry carries none at its edges.
fn prose(text: &str) -> String {
    escape::markdown(text).trim_matches('\n').to_string()
}

fn tool(name: &str, summary: &str, _output: Option<&str>, failed: bool, _opts: &Options) -> String {
    let mut line = format!("- **{name}**");
    let span = escape::code_span(summary);
    if !span.is_empty() {
        line.push(' ');
        line.push_str(&span);
    }
    if failed {
        line.push_str(" (failed)");
    }
    line
}

fn callout(entry: &Entry, opts: &Options) -> String {
    let (kind, header, text) = match entry {
        Entry::User { at, text } => ("NOTE", format!("**User**{}", label(*at, &opts.tz)), text),
        Entry::AgentReport { at, name, text } => (
            "TIP",
            format!("**Agent: {name}**{}", label(*at, &opts.tz)),
            text,
        ),
        _ => unreachable!("run entries are never callouts"),
    };
    let mut blocks = escape::blocks(text);
    let mut head = format!("> [!{kind}]\n> {header}");
    if matches!(blocks.first(), Some(Block::Prose(_)))
        && let Block::Prose(prose) = blocks.remove(0)
    {
        head.push_str("\n>\n");
        head.push_str(&quote(&prose));
    }
    let mut parts = vec![head];
    parts.extend(blocks.into_iter().map(|block| match block {
        Block::Prose(prose) => quote(&prose),
        Block::Fence(fence) => fence,
    }));
    parts.join("\n\n")
}

fn quote(prose: &str) -> String {
    prose
        .split('\n')
        .map(|l| {
            if l.trim().is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn label(at: Option<Timestamp>, tz: &TimeZone) -> String {
    at.map(|at| format!(" `{}`", at.to_zoned(tz.clone()).strftime("%m/%d %H:%M:%S")))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use jiff::tz::{Offset, TimeZone};

    use super::*;
    use crate::harness::Harness;
    use crate::session::Entry;

    fn opts() -> Options {
        Options {
            thinking: true,
            tool_output: true,
            max_lines: 100,
            tz: TimeZone::fixed(Offset::constant(-4)),
        }
    }

    fn ts(s: &str) -> Option<Timestamp> {
        Some(s.parse().unwrap())
    }

    fn session(entries: Vec<Entry>) -> Session {
        let mut s = Session::new(Harness::Claude, "t.jsonl".into());
        s.entries = entries;
        s
    }

    fn user(at: Option<Timestamp>, text: &str) -> Entry {
        Entry::User {
            at,
            text: text.into(),
        }
    }

    fn assistant(at: Option<Timestamp>, text: &str) -> Entry {
        Entry::Assistant {
            at,
            text: text.into(),
        }
    }

    fn thinking(at: Option<Timestamp>, text: &str) -> Entry {
        Entry::Thinking {
            at,
            text: text.into(),
        }
    }

    fn report(at: Option<Timestamp>, name: &str, text: &str) -> Entry {
        Entry::AgentReport {
            at,
            name: name.into(),
            text: text.into(),
        }
    }

    fn tool(name: &str, summary: &str, output: Option<&str>, failed: bool) -> Entry {
        Entry::Tool {
            at: None,
            name: name.into(),
            summary: summary.into(),
            output: output.map(String::from),
            failed,
        }
    }

    #[test]
    fn runs() {
        let s = session(vec![
            assistant(ts("2026-09-04T02:25:00Z"), "a1"),
            user(None, "u"),
            assistant(None, "a2"),
            report(None, "r", "rep"),
            tool("Bash", "ls", None, false),
            assistant(None, "a3"),
        ]);
        insta::assert_snapshot!("runs", body(&s, &opts()));
    }

    #[test]
    fn blank_entries() {
        let s = session(vec![
            user(None, ""),
            assistant(None, ""),
            thinking(None, "  \n"),
            assistant(None, "kept"),
            user(None, " \n\t"),
        ]);
        insta::assert_snapshot!("blank_entries", body(&s, &opts()));
    }

    #[test]
    fn callout_fences() {
        let s = session(vec![
            user(None, "before\n```sh\nls <dir>\n```\nafter"),
            report(None, "agent", "```\nx\n```\ntail"),
            user(None, "```\nonly\n```"),
        ]);
        insta::assert_snapshot!("callout_fences", body(&s, &opts()));
    }

    #[test]
    fn thinking_only_run() {
        let s = session(vec![
            user(None, "one"),
            thinking(ts("2026-09-04T02:25:00Z"), "hidden"),
            user(None, "two"),
        ]);
        let opts = Options {
            thinking: false,
            ..opts()
        };
        insta::assert_snapshot!("thinking_only_run", body(&s, &opts));
    }
}
