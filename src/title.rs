//! Note titles and filename slugs, derived from an adapter's `title_source`.

const NO_TITLE: &str = "(no title)";
const MAX_TITLE_LEN: usize = 80;

pub fn title(source: Option<&str>) -> String {
    derive(source).unwrap_or_else(|| NO_TITLE.to_string())
}

fn derive(source: Option<&str>) -> Option<String> {
    let collapsed = source?.split_whitespace().collect::<Vec<_>>().join(" ");
    let candidate = note_name(&collapsed).unwrap_or(&collapsed).trim();
    (!candidate.is_empty()).then(|| truncate(candidate))
}

/// Sessions opened on a vault note are titled by the note's name.
fn note_name(s: &str) -> Option<&str> {
    let path = s.strip_suffix(".md").filter(|p| !p.is_empty())?;
    Some(path.rsplit('/').next().unwrap_or(path))
}

fn truncate(s: &str) -> String {
    if s.len() <= MAX_TITLE_LEN {
        return s.to_string();
    }
    let mut cut = MAX_TITLE_LEN;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    if let Some(space) = s[..cut].rfind(' ').filter(|&i| i > 0) {
        cut = space;
    }
    format!("{}...", s[..cut].trim_end_matches(' '))
}

const MAX_SLUG_LEN: usize = 50;

pub fn slug(source: Option<&str>) -> String {
    derive(source).map(|t| slugify(&t)).unwrap_or_default()
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        // ASCII-only lowercasing keeps non-ASCII letters out of the slug.
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-');
    if out.len() <= MAX_SLUG_LEN {
        return out.to_string();
    }
    let mut clipped = &out[..MAX_SLUG_LEN];
    if let Some(hyphen) = clipped.rfind('-').filter(|&i| i > 0) {
        clipped = &clipped[..hyphen];
    }
    clipped.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_paths_title_by_note_name() {
        for (source, want) in [
            (
                "Areas/garden/Compare seed-starting trays.md",
                "Compare seed-starting trays",
            ),
            // A typo is reproduced, not corrected.
            (
                "gAreas/garden/Soil test - week 3 check.md",
                "Soil test - week 3 check",
            ),
            (
                "Projects/demo/Rename the sample config loader.md",
                "Rename the sample config loader",
            ),
            ("Areas/x/Plan.md  \n", "Plan"),
        ] {
            assert_eq!(title(Some(source)), want, "{source:?}");
        }
    }

    #[test]
    fn prose_is_kept_with_whitespace_collapsed() {
        assert_eq!(
            title(Some("openssl command to view https cert details")),
            "openssl command to view https cert details"
        );
        assert_eq!(title(Some("a   b\n\nc")), "a b c");
        assert_eq!(title(Some("short one")), "short one");
    }

    #[test]
    fn nothing_usable_gives_the_sentinel() {
        for source in [None, Some(""), Some("  \n\t "), Some("foo/.md")] {
            assert_eq!(title(source), NO_TITLE, "{source:?}");
        }
    }

    #[test]
    fn bare_md_suffix_is_not_a_path() {
        assert_eq!(title(Some(".md")), ".md");
    }

    #[test]
    fn long_titles_cut_at_a_word_boundary() {
        let long = "this is a very long first message that goes well past the eighty character limit and keeps going";
        assert_eq!(
            title(Some(long)),
            "this is a very long first message that goes well past the eighty character..."
        );
    }

    #[test]
    fn long_token_cuts_at_a_char_boundary() {
        let long = format!("{}éb", "a".repeat(79));
        assert_eq!(title(Some(&long)), format!("{}...", "a".repeat(79)));
    }

    #[test]
    fn slug_cases() {
        for (source, want) in [
            ("", ""),
            ("  \n\t ", ""),
            ("hello world", "hello-world"),
            ("Fix the bug — now!", "fix-the-bug-now"),
            ("already-a-slug", "already-a-slug"),
            ("---foo---bar---", "foo-bar"),
            ("café résumé", "caf-r-sum"),
            ("ship it 🚢🚢🚢", "ship-it"),
            ("!@#$%^&*()", ""),
            (
                "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda",
                "alpha-beta-gamma-delta-epsilon-zeta-eta-theta",
            ),
            // Unicode lowercasing maps KELVIN SIGN to ASCII `k`.
            ("\u{212A} \u{212A} \u{212A}", ""),
            ("修正してください", ""),
        ] {
            assert_eq!(slug(Some(source)), want, "{source:?}");
        }
    }

    #[test]
    fn slug_follows_the_derived_title() {
        assert_eq!(
            slug(Some("Projects/demo/Rename the sample config loader.md")),
            "rename-the-sample-config-loader"
        );
        assert_eq!(slug(Some(".md")), "md");
        assert_eq!(slug(Some("foo/.md")), "");
        assert_eq!(slug(None), "");
    }

    #[test]
    fn truncated_title_slug_has_no_trailing_hyphen() {
        let long = "fix the thing!!! ".repeat(8);
        assert!(title(Some(&long)).ends_with("..."));
        let s = slug(Some(&long));
        assert!(!s.ends_with('-') && s.len() <= 50, "{s:?}");
    }

    #[test]
    fn non_ascii_title_is_kept_with_an_empty_slug() {
        assert_eq!(title(Some("修正してください")), "修正してください");
        assert_eq!(slug(Some("修正してください")), "");
    }
}
