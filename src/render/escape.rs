//! Obsidian-safe markdown from harness text.

const MAX_SUMMARY_LEN: usize = 80;

#[derive(Debug, PartialEq, Eq)]
pub enum Block {
    /// Escaped prose, one or more whole lines.
    Prose(String),
    /// A fenced code block, verbatim, always closed.
    Fence(String),
}

/// Text for an unquoted entry.
pub fn markdown(text: &str) -> String {
    segments(text)
        .into_iter()
        .map(|segment| match segment {
            Segment::Prose(lines) => prose(&lines),
            Segment::Fence(fence) => fence,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text for a callout, split so fences can be emitted outside the quote.
pub fn blocks(text: &str) -> Vec<Block> {
    segments(text)
        .into_iter()
        .filter_map(|segment| match segment {
            Segment::Prose(lines) => {
                let lines = trim_blank(&lines);
                (!lines.is_empty()).then(|| Block::Prose(prose(lines)))
            }
            Segment::Fence(fence) => Some(Block::Fence(fence)),
        })
        .collect()
}

pub fn fence_for(content: &str) -> String {
    "`".repeat(longest_backticks(content).max(2) + 1)
}

pub fn code_span(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }
    let body = truncate(&collapsed, MAX_SUMMARY_LEN);
    let delimiter = "`".repeat(longest_backticks(&body) + 1);
    // A span that starts or ends with a backtick would merge with the delimiter.
    let pad = if body.starts_with('`') || body.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{delimiter}{pad}{body}{pad}{delimiter}")
}

/// `max == 0` means no limit.
pub fn truncate_lines(text: &str, max: usize) -> String {
    if max == 0 {
        return text.to_string();
    }
    match text.match_indices('\n').nth(max - 1) {
        Some((end, _)) => format!("{}\n... (truncated)", &text[..end]),
        None => text.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &s[..cut])
}

fn longest_backticks(s: &str) -> usize {
    s.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

fn run_len(s: &str, ch: char) -> usize {
    s.len() - s.trim_start_matches(ch).len()
}

enum Segment<'a> {
    Prose(Vec<&'a str>),
    Fence(String),
}

struct Opener {
    indent: usize,
    ch: char,
    len: usize,
}

fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut prose = Vec::new();
    let mut lines = text.split('\n');
    while let Some(line) = lines.next() {
        let Some(open) = opener(line) else {
            prose.push(line);
            continue;
        };
        if !prose.is_empty() {
            out.push(Segment::Prose(std::mem::take(&mut prose)));
        }
        let mut fence = dedent(line, open.indent).to_string();
        let mut closed = false;
        for line in lines.by_ref() {
            fence.push('\n');
            fence.push_str(dedent(line, open.indent));
            if closes(line, &open) {
                closed = true;
                break;
            }
        }
        if !closed {
            fence.push('\n');
            fence.extend(std::iter::repeat_n(open.ch, open.len));
        }
        out.push(Segment::Fence(fence));
    }
    if !prose.is_empty() {
        out.push(Segment::Prose(prose));
    }
    out
}

fn opener(line: &str) -> Option<Opener> {
    let body = line.trim_start_matches([' ', '\t']);
    let ch = body.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = run_len(body, ch);
    // A backtick in the info string makes the line an inline code span.
    if len < 3 || (ch == '`' && body[len..].contains('`')) {
        return None;
    }
    Some(Opener {
        indent: line.len() - body.len(),
        ch,
        len,
    })
}

fn closes(line: &str, open: &Opener) -> bool {
    let body = line.trim_start_matches([' ', '\t']);
    let len = run_len(body, open.ch);
    len >= open.len && body[len..].trim().is_empty()
}

/// Leading whitespace would stop a fence from being a fence at column 0.
fn dedent(line: &str, indent: usize) -> &str {
    let ws = line.len() - line.trim_start_matches([' ', '\t']).len();
    &line[ws.min(indent)..]
}

fn trim_blank<'a>(lines: &'a [&'a str]) -> &'a [&'a str] {
    let start = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(start, |i| i + 1);
    &lines[start..end]
}

fn prose(lines: &[&str]) -> String {
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_for_outgrows_backtick_runs() {
        for (content, want) in [
            ("a\nb", "```"),
            ("", "```"),
            ("# Doc\n\n```markdown\n## Heading\n```\n\nmore text", "````"),
            ("before\n````\nnested\n````\nafter", "`````"),
        ] {
            assert_eq!(fence_for(content), want, "content: {content:?}");
        }
    }

    #[test]
    fn code_span_collapses_and_delimits() {
        for (text, want) in [
            ("ls -la", "`ls -la`"),
            (
                "jj commit -m \"feat: add thing\n\nLong body line one.\"",
                "`jj commit -m \"feat: add thing Long body line one.\"`",
            ),
            ("  \n\t ", ""),
            ("echo `date` now", "``echo `date` now``"),
            ("echo `date`", "`` echo `date` ``"),
            ("```", "```` ``` ````"),
        ] {
            assert_eq!(code_span(text), want, "text: {text:?}");
        }
    }

    #[test]
    fn code_span_truncates_at_80_bytes() {
        let long = "a".repeat(100);
        assert_eq!(code_span(&long), format!("`{}...`", "a".repeat(80)));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        for (s, max, want) in [
            ("short", 10, "short"),
            ("12345", 5, "12345"),
            ("hello world", 5, "hello..."),
            ("a…bcdef", 2, "a..."),
            ("🕓🕓", 6, "🕓..."),
        ] {
            assert_eq!(truncate(s, max), want, "s: {s:?}");
        }
    }

    #[test]
    fn truncate_lines_cuts_after_max() {
        for (text, max, want) in [
            ("line1\nline2\nline3", 5, "line1\nline2\nline3"),
            ("a\nb\nc", 3, "a\nb\nc"),
            ("a\nb\nc\nd\ne", 2, "a\nb\n... (truncated)"),
            ("hello", 1, "hello"),
            ("a\nb\nc", 0, "a\nb\nc"),
        ] {
            assert_eq!(truncate_lines(text, max), want, "text: {text:?}");
        }
    }

    fn check(cases: &[(&str, &str)]) {
        for &(input, want) in cases {
            assert_eq!(markdown(input), want, "input: {input:?}");
        }
    }

    #[test]
    fn fences_are_closed_and_dedented() {
        check(&[
            ("~~~\n<a>\n~~~", "~~~\n<a>\n~~~"),
            ("````\n```\n<a>\n```\n````", "````\n```\n<a>\n```\n````"),
            ("  ```sh\n  ls <dir>\n  ```", "```sh\nls <dir>\n```"),
            (
                "1. step\n    ```sh\n    ls <dir>\n    ```\n2. next",
                "1. step\n```sh\nls <dir>\n```\n2. next",
            ),
            ("```yaml\nkey: 1\n---\n```", "```yaml\nkey: 1\n---\n```"),
            (
                "text\n```rust\nfn main() {}",
                "text\n```rust\nfn main() {}\n```",
            ),
            ("~~~~\nx", "~~~~\nx\n~~~~"),
            ("```\r\n<a>\r\n```\r\nb", "```\r\n<a>\r\n```\r\nb"),
        ]);
    }

    #[test]
    fn empty_and_blank_text() {
        assert_eq!(markdown(""), "");
        assert_eq!(markdown(" \n "), " \n ");
        assert_eq!(blocks(""), vec![]);
        assert_eq!(blocks(" \n\t\n"), vec![]);
    }

    #[test]
    fn blocks_split_prose_and_fences() {
        let p = |s: &str| Block::Prose(s.to_string());
        let f = |s: &str| Block::Fence(s.to_string());
        for (text, want) in [
            (
                "hi\n```\ncode\n```\nbye",
                vec![p("hi"), f("```\ncode\n```"), p("bye")],
            ),
            (
                "```\na\n```\n```\nb\n```",
                vec![f("```\na\n```"), f("```\nb\n```")],
            ),
            (
                "```\na\n```\n\n  \n```\nb\n```",
                vec![f("```\na\n```"), f("```\nb\n```")],
            ),
            ("```\na\n```\ntext", vec![f("```\na\n```"), p("text")]),
            ("text\n```\na\n```", vec![p("text"), f("```\na\n```")]),
            ("\n\nhi\n\n```\nx\n```", vec![p("hi"), f("```\nx\n```")]),
            (
                "note:\n    ```\n    <a>\n    ```",
                vec![p("note:"), f("```\n<a>\n```")],
            ),
            ("```ls```\nthen", vec![p("```ls```\nthen")]),
        ] {
            assert_eq!(blocks(text), want, "text: {text:?}");
        }
        assert_eq!(markdown("\n\nhi\n\n```\nx\n```"), "\n\nhi\n\n```\nx\n```");
    }
}
