//! Obsidian-safe markdown from harness text.

const MAX_SUMMARY_LEN: usize = 80;

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
}
