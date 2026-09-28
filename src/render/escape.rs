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
    let text = normalize_newlines(text);
    segments(&text)
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
    let text = normalize_newlines(text);
    segments(&text)
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
    "`".repeat(longest_run(content, '`').max(2) + 1)
}

pub fn code_span(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }
    let body = truncate(&collapsed, MAX_SUMMARY_LEN);
    let delimiter = "`".repeat(longest_run(&body, '`') + 1);
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

fn longest_run(s: &str, ch: char) -> usize {
    s.split(|c| c != ch).map(str::len).max().unwrap_or(0)
}

/// Rust's `trim` also strips Unicode spaces such as NBSP, which CommonMark
/// treats as text.
const MD_SPACE: [char; 2] = [' ', '\t'];

/// CommonMark ends a line at `\r` too; a scanner splitting only on `\n` would
/// match code spans across lines Obsidian has already split.
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_blank(s: &str) -> bool {
    s.trim_matches(MD_SPACE).is_empty()
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
    width: usize,
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
        let mut body = Vec::new();
        for line in lines.by_ref() {
            if closes(line, &open) {
                break;
            }
            body.push(dedent(line, open.indent));
        }
        out.push(Segment::Fence(fence(line, &open, &body)));
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
        width: indent_width(line),
        ch,
        len,
    })
}

/// The delimiter is rebuilt longer than any run in the content, so no content
/// line can close it, however dedenting shifts it.
fn fence(opener: &str, open: &Opener, body: &[&str]) -> String {
    let info = &opener.trim_start_matches([' ', '\t'])[open.len..];
    let longest = body
        .iter()
        .map(|l| longest_run(l, open.ch))
        .max()
        .unwrap_or(0);
    let run = open.ch.to_string().repeat(open.len.max(longest + 1));
    let mut out = format!("{run}{info}");
    for line in body {
        out.push('\n');
        out.push_str(line);
    }
    out.push('\n');
    out.push_str(&run);
    out
}

/// CommonMark reads a run indented 4+ columns past the margin as content.
fn closes(line: &str, open: &Opener) -> bool {
    let body = line.trim_start_matches([' ', '\t']);
    let len = run_len(body, open.ch);
    indent_width(line) <= open.width + 3 && len >= open.len && is_blank(&body[len..])
}

fn indent_width(line: &str) -> usize {
    let mut width = 0;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += 4 - width % 4,
            _ => break,
        }
    }
    width
}

/// Leading whitespace would stop a fence from being a fence at column 0.
fn dedent(line: &str, indent: usize) -> &str {
    let ws = line.len() - line.trim_start_matches([' ', '\t']).len();
    &line[ws.min(indent)..]
}

fn trim_blank<'a>(lines: &'a [&'a str]) -> &'a [&'a str] {
    let start = lines
        .iter()
        .position(|l| !is_blank(l))
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !is_blank(l))
        .map_or(start, |i| i + 1);
    &lines[start..end]
}

/// Code spans are matched within a line. A span crossing lines could run past
/// a block CommonMark ends first, such as an HTML block, and copy it raw.
fn prose(lines: &[&str]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if i > 0 && !is_blank(lines[i - 1]) && is_underline(line) {
            out.push_str(&underline(line));
        } else {
            inline(line, &mut out);
        }
    }
    out
}

fn is_underline(line: &str) -> bool {
    let body = line.trim_start_matches(' ');
    let run = body.trim_end_matches(MD_SPACE);
    line.len() - body.len() <= 3
        && !run.is_empty()
        && (run.bytes().all(|b| b == b'-') || run.bytes().all(|b| b == b'='))
}

fn underline(line: &str) -> String {
    let run = line.trim_matches(MD_SPACE);
    if run.len() >= 3 && run.starts_with('-') {
        "<hr>".to_string()
    } else {
        format!("\\{run}")
    }
}

fn inline(text: &str, out: &mut String) {
    let mut i = 0;
    let mut after_space = true;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        match c {
            '`' => {
                let n = run_len(rest, '`');
                let len = closing_run(&rest[n..], n).map_or(n, |end| n + end + n);
                out.push_str(&rest[..len]);
                i += len;
                after_space = false;
                continue;
            }
            '<' | '>' => {
                // An odd backslash run would otherwise escape the entity's `&`.
                if text[..i].bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1 {
                    out.push('\\');
                }
                out.push_str(if c == '<' { "&lt;" } else { "&gt;" });
            }
            '#' if after_space && rest[1..].chars().next().is_some_and(is_tag_char) => {
                out.push_str("\\#");
            }
            _ => out.push(c),
        }
        after_space = c.is_whitespace();
        i += c.len_utf8();
    }
}

/// CommonMark closes a span only on a run of equal length.
fn closing_run(s: &str, n: usize) -> Option<usize> {
    let mut i = 0;
    while let Some(offset) = s[i..].find('`') {
        let start = i + offset;
        let len = run_len(&s[start..], '`');
        if len == n {
            return Some(start);
        }
        i = start + len;
    }
    None
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
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
            ("```\r\n<a>\r\n```\r\nb", "```\n<a>\n```\nb"),
        ]);
    }

    #[test]
    fn closer_indent_follows_commonmark() {
        check(&[
            (
                "```\ncode\n    ```\n<tag>\n```\nafter <x>",
                "````\ncode\n    ```\n<tag>\n````\nafter &lt;x&gt;",
            ),
            (
                "```\ncode\n    ```\nafter <x>",
                "````\ncode\n    ```\nafter <x>\n````",
            ),
            ("```\nx\n\t```\ny", "````\nx\n\t```\ny\n````"),
            ("```\nx\n   ```\ny <z>", "```\nx\n```\ny &lt;z&gt;"),
            ("  ```\n  a\n     ```\nb", "```\na\n```\nb"),
        ]);
    }

    #[test]
    fn content_cannot_close_the_emitted_fence() {
        check(&[(
            " \t```\ncode\n\t    ```\n<script>x</script>\n \t```\nafter <x>",
            "````\ncode\n   ```\n<script>x</script>\n````\nafter &lt;x&gt;",
        )]);
    }

    #[test]
    fn only_commonmark_whitespace_is_blank() {
        check(&[
            (
                "```\nx\n```\u{a0}\n<t>\n```\nafter <x>",
                "````\nx\n```\u{a0}\n<t>\n````\nafter &lt;x&gt;",
            ),
            ("a\n---\u{a0}", "a\n---\u{a0}"),
            ("\u{a0}\n---", "\u{a0}\n<hr>"),
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

    #[test]
    fn cc2md_golden_cases() {
        check(&[
            (
                "the trigger (\"let's work on @<task>\")",
                "the trigger (\"let's work on @&lt;task&gt;\")",
            ),
            (
                "are you sure it's not the @<task> tag?",
                "are you sure it's not the @&lt;task&gt; tag?",
            ),
            (
                "resolve `<path>` against root",
                "resolve `<path>` against root",
            ),
            (
                "before\n```\n<task> not html here\n```\nafter <task> here",
                "before\n```\n<task> not html here\n```\nafter &lt;task&gt; here",
            ),
            ("first\n---\nsecond", "first\n<hr>\nsecond"),
        ]);
    }

    #[test]
    fn cc2md_bugs_are_fixed() {
        check(&[
            // A lone backtick does not disable escaping in later paragraphs.
            (
                "it's a ` tick\n\nthen <x>",
                "it's a ` tick\n\nthen &lt;x&gt;",
            ),
            ("~~~\n<a>\n~~~\n<b>", "~~~\n<a>\n~~~\n&lt;b&gt;"),
            (
                "````\n```\n<a>\n```\n````\n<b>",
                "````\n```\n<a>\n```\n````\n&lt;b&gt;",
            ),
            (
                "1. step\n    ```sh\n    ls <dir>\n    ```\n2. next <x>",
                "1. step\n```sh\nls <dir>\n```\n2. next &lt;x&gt;",
            ),
            ("a\n---", "a\n<hr>"),
            (r"match \<word\>", r"match \\&lt;word\\&gt;"),
            (r"even \\<x>", r"even \\&lt;x&gt;"),
        ]);
    }

    #[test]
    fn tags_are_escaped_outside_code() {
        check(&[
            ("#todo at start", r"\#todo at start"),
            ("see #todo and\t#done", "see \\#todo and\t\\#done"),
            ("issue #123", r"issue \#123"),
            ("path #a/b", r"path \#a/b"),
            ("  #indented", r"  \#indented"),
            ("#日本", r"\#日本"),
            ("# Heading\n## Sub", "# Heading\n## Sub"),
            (
                "foo#bar and https://x.io/a#frag",
                "foo#bar and https://x.io/a#frag",
            ),
            ("#!/bin/sh", "#!/bin/sh"),
            ("use `#tag` here", "use `#tag` here"),
            ("```\n#tag\n```", "```\n#tag\n```"),
        ]);
    }

    #[test]
    fn multibyte_text_around_escapes() {
        check(&[("é<x>é #ü", "é&lt;x&gt;é \\#ü")]);
    }

    #[test]
    fn setext_underlines_are_neutralised() {
        check(&[
            ("a\n---\nb", "a\n<hr>\nb"),
            ("a\n  -----  ", "a\n<hr>"),
            ("a\n\n---\nb", "a\n\n---\nb"),
            ("---\nb", "---\nb"),
            ("```\nx\n```\n---", "```\nx\n```\n---"),
            ("a\n--", "a\n\\--"),
            ("a\n===", "a\n\\==="),
            ("a\n    ---", "a\n    ---"),
            ("a\r\n---\r\nb", "a\n<hr>\nb"),
        ]);
    }

    #[test]
    fn code_spans_stay_on_one_line() {
        check(&[
            ("a ` b\n---\n<x>`", "a ` b\n<hr>\n&lt;x&gt;`"),
            ("a ` b\n# H <x> `", "a ` b\n# H &lt;x&gt; `"),
            ("a ` b\n- item <x> `", "a ` b\n- item &lt;x&gt; `"),
            ("a ` b\n2) item <x> `", "a ` b\n2) item &lt;x&gt; `"),
            ("a ` b\n> q <x> `", "a ` b\n&gt; q &lt;x&gt; `"),
            ("# H `\n<task>`", "# H `\n&lt;task&gt;`"),
            (
                "before `\n<div>\n</div>\n`",
                "before `\n&lt;div&gt;\n&lt;/div&gt;\n`",
            ),
            ("a ` b\n***\n<x> `", "a ` b\n***\n&lt;x&gt; `"),
            // A span wrapped across lines shows its escapes, but hides nothing.
            ("a `x\n<y>` b", "a `x\n&lt;y&gt;` b"),
            ("a ``x ` <y>`` <z>", "a ``x ` <y>`` &lt;z&gt;"),
        ]);
    }

    #[test]
    fn inline_triple_backticks_are_not_a_fence() {
        check(&[("```ls <dir>```\nthen <x>", "```ls <dir>```\nthen &lt;x&gt;")]);
    }

    #[test]
    fn bare_carriage_returns_end_lines() {
        check(&[
            (
                "before `\r<div>\r</div>\r`",
                "before `\n&lt;div&gt;\n&lt;/div&gt;\n`",
            ),
            ("```\r<a>\r```\rb <x>", "```\n<a>\n```\nb &lt;x&gt;"),
        ]);
    }

    #[test]
    fn crlf_prose_is_escaped() {
        check(&[("```\r\n<a>\r\n```\r\nb <x>", "```\n<a>\n```\nb &lt;x&gt;")]);
    }

    #[test]
    fn blocks_escape_prose_only() {
        let p = |s: &str| Block::Prose(s.to_string());
        let f = |s: &str| Block::Fence(s.to_string());
        assert_eq!(
            blocks("hi <x>\n```\n<a>\n```\n#tag"),
            vec![p("hi &lt;x&gt;"), f("```\n<a>\n```"), p(r"\#tag")],
        );
        assert_eq!(
            blocks("are you sure it's not the @<task> tag?"),
            vec![p("are you sure it's not the @&lt;task&gt; tag?")],
        );
    }
}
