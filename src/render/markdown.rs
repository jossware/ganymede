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
/// An indented first line would continue a preceding tool's list item.
fn prose(text: &str) -> String {
    escape::markdown(text)
        .trim_start_matches([' ', '\t', '\n'])
        .trim_end_matches('\n')
        .to_string()
}

fn tool(name: &str, summary: &str, output: Option<&str>, failed: bool, opts: &Options) -> String {
    // An empty name would render as `****`, a thematic break.
    let mut line = if name.trim().is_empty() {
        "- *(unnamed)*".to_string()
    } else {
        format!("- **{}**", escape::label(name))
    };
    let span = escape::code_span(summary);
    if !span.is_empty() {
        line.push(' ');
        line.push_str(&span);
    }
    if failed {
        line.push_str(" (failed)");
    }
    let Some(output) = output.filter(|_| opts.tool_output) else {
        return line;
    };
    let output = escape::truncate_lines(output, opts.max_lines);
    let output = output.trim_end_matches('\n');
    if output.is_empty() {
        return line;
    }
    let fence = escape::fence_for(output);
    // Indented, the fence stays inside the list item and the tools stay one list.
    let block = format!("{fence}\n{output}\n{fence}")
        .split('\n')
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("  {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{line}\n\n{block}")
}

fn callout(entry: &Entry, opts: &Options) -> String {
    let (kind, header, text) = match entry {
        Entry::User { at, text } => ("NOTE", format!("**User**{}", label(*at, &opts.tz)), text),
        Entry::AgentReport { at, name, text } => (
            "TIP",
            format!("**Agent: {}**{}", escape::label(name), label(*at, &opts.tz)),
            text,
        ),
        _ => unreachable!("run entries are never callouts"),
    };
    let mut blocks = escape::blocks(text).into_iter().peekable();
    let mut head = format!("> [!{kind}]\n> {header}");
    if let Some(Block::Prose(prose)) = blocks.next_if(|b| matches!(b, Block::Prose(_))) {
        head.push_str("\n>\n");
        head.push_str(&quote(&prose));
    }
    let mut parts = vec![head];
    parts.extend(blocks.map(|block| match block {
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
            user(
                ts("2026-09-04T02:25:00Z"),
                "Add a greeting to the sample CLI",
            ),
            thinking(
                ts("2026-09-04T02:25:02.274Z"),
                "The user wants a greeting. Look at main.rs first.",
            ),
            Entry::Tool {
                at: ts("2026-09-04T02:25:04.548Z"),
                name: "Read".into(),
                summary: "/Users/user/Workspaces/sample/src/main.rs".into(),
                output: Some("1\tfn main() {}\n".into()),
                failed: false,
            },
            Entry::Tool {
                at: ts("2026-09-04T02:25:06.822Z"),
                name: "Bash".into(),
                summary: "cargo test".into(),
                output: Some("error: could not compile `sample`".into()),
                failed: true,
            },
            Entry::Tool {
                at: ts("2026-09-04T02:25:09.233Z"),
                name: "Edit".into(),
                summary: "/Users/user/Worktrees/sample/feature/src/main.rs".into(),
                output: Some("The file has been updated.".into()),
                failed: false,
            },
            assistant(
                ts("2026-09-04T02:25:11.507Z"),
                "Added a `println!` greeting. Tests failed to compile before the edit.",
            ),
        ];
        s
    }

    #[test]
    fn basic_note() {
        let note = crate::render::note(&basic(), "sample", Some("feature"), &opts());
        insta::assert_snapshot!("basic", note);
    }

    #[test]
    fn hook_flags() {
        let opts = Options {
            thinking: false,
            tool_output: false,
            ..opts()
        };
        insta::assert_snapshot!("hook_flags", body(&basic(), &opts));
    }

    #[test]
    fn tool_edges() {
        let s = session(vec![
            tool("Glob", "", None, false),
            tool("Bash", "echo `x`", Some("ok"), false),
            tool("Read", "f.md", Some("````\ncode"), false),
            tool("Bash", "seq", Some("1\n2\n3\n4"), false),
            tool("Write", "w", Some(""), false),
            tool("Bash", "nl", Some("out\n\n"), false),
            tool("Agent", "", None, true),
        ]);
        let opts = Options {
            max_lines: 2,
            ..opts()
        };
        insta::assert_snapshot!("tool_edges", body(&s, &opts));
    }

    #[test]
    fn tool_output_without_limit() {
        let s = session(vec![tool("Bash", "seq", Some("1\n2\n3\n4"), false)]);
        let opts = Options {
            max_lines: 0,
            ..opts()
        };
        assert!(body(&s, &opts).contains("  1\n  2\n  3\n  4\n  ```"));
    }

    #[test]
    fn tool_output_crlf_becomes_lf() {
        let s = session(vec![tool("Bash", "type", Some("a\r\nb\r\n"), false)]);
        assert_eq!(
            body(&s, &opts()),
            "> **Claude**\n\n- **Bash** `type`\n\n  ```\n  a\n  b\n  ```\n"
        );
    }

    #[test]
    fn tool_output_bare_cr_stays_inside_the_fence() {
        let s = session(vec![
            tool("Bash", "curl", Some("  0%\r 50%\r100%\ndone"), false),
            tool("Bash", "ls", Some("ok"), false),
        ]);
        let opts = Options {
            max_lines: 3,
            ..opts()
        };
        assert_eq!(
            body(&s, &opts),
            "> **Claude**\n\n- **Bash** `curl`\n\n  ```\n    0%\n   50%\n  100%\n  ... (truncated)\n  ```\n\n- **Bash** `ls`\n\n  ```\n  ok\n  ```\n"
        );
    }

    #[test]
    fn agent_name_stays_in_the_header() {
        let s = session(vec![report(None, "a**b\n# heading", "rep")]);
        assert_eq!(
            body(&s, &opts()),
            "> [!TIP]\n> **Agent: a\\*\\*b \\# heading**\n>\n> rep\n"
        );
    }

    #[test]
    fn tool_name_stays_in_the_header() {
        let s = session(vec![tool("mcp__srv__x*y\n# h", "", None, false)]);
        assert_eq!(
            body(&s, &opts()),
            "> **Claude**\n\n- **mcp\\_\\_srv\\_\\_x\\*y \\# h**\n"
        );
    }

    #[test]
    fn empty_tool_name_gets_a_placeholder() {
        let s = session(vec![
            tool("", "ls", None, false),
            tool(" \t", "", None, false),
        ]);
        assert_eq!(
            body(&s, &opts()),
            "> **Claude**\n\n- *(unnamed)* `ls`\n\n- *(unnamed)*\n"
        );
    }

    #[test]
    fn indented_prose_after_a_tool_leaves_the_list() {
        let s = session(vec![
            tool("Bash", "ls", None, false),
            assistant(None, "\n  \n  done\n  indented"),
        ]);
        assert_eq!(
            body(&s, &opts()),
            "> **Claude**\n\n- **Bash** `ls`\n\ndone\n  indented\n"
        );
    }

    #[test]
    fn codex_run_header() {
        let mut s = session(vec![assistant(ts("2026-09-04T02:25:00Z"), "a")]);
        s.harness = Harness::Codex;
        assert_eq!(body(&s, &opts()), "> **Codex** `09/03 22:25:00`\n\na\n");
    }
}
