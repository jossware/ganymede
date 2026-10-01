//! `ganymede errors`: recent error-log entries, one readable line each.

use std::io::Write;
use std::process::ExitCode;

use jiff::tz::TimeZone;

use crate::errlog::{self, Entry};
use crate::paths::{self, Env};

pub fn run(count: usize, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let Some(state) = &env.state else {
        let _ = writeln!(err, "ganymede: HOME is not set");
        return ExitCode::FAILURE;
    };
    let entries = match errlog::recent(&paths::error_log(state), count) {
        Ok(entries) => entries,
        Err(e) => {
            let _ = writeln!(err, "ganymede: {e}");
            return ExitCode::FAILURE;
        }
    };
    let tz = TimeZone::system();
    for entry in &entries {
        if let Err(e) = writeln!(out, "{}", line(entry, &tz)) {
            let _ = writeln!(err, "ganymede: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// `<date> <time> <stage> <harness> <event> <session8> <error>`.
fn line(entry: &Entry, tz: &TimeZone) -> String {
    let session = entry
        .session
        .as_ref()
        .map(|s| s.chars().take(8).collect::<String>());
    format!(
        "{} {} {} {} {} {}",
        entry.ts.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M"),
        entry.stage.as_str(),
        field(entry.harness.as_deref()),
        field(entry.event.as_deref()),
        field(session.as_deref()),
        errlog::one_line(&entry.error),
    )
}

/// One whitespace-free column. Payload fields are not trusted to be
/// single words, and a space would shift every column after it.
fn field(value: Option<&str>) -> String {
    let words: Vec<&str> = value.unwrap_or("").split_whitespace().collect();
    if words.is_empty() {
        "-".to_string()
    } else {
        words.join("_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errlog::Stage;

    fn entry() -> Entry {
        let mut entry = Entry::new(Stage::Parse, "missing\n  session_meta".into());
        entry.ts = "2026-10-01T15:40:02Z".parse().unwrap();
        entry
    }

    #[test]
    fn prints_every_field() {
        let mut entry = entry();
        entry.harness = Some("codex".into());
        entry.event = Some("Stop".into());
        entry.session = Some("019a3f2c-0000-7000-8000-000000000001".into());
        assert_eq!(
            line(&entry, &TimeZone::fixed(jiff::tz::offset(-4))),
            "2026-10-01 11:40 parse codex Stop 019a3f2c missing session_meta"
        );
    }

    #[test]
    fn keeps_payload_fields_on_one_line() {
        let mut entry = entry();
        entry.harness = Some(" ".into());
        entry.event = Some("Stop\nspoof".into());
        entry.session = Some("ab\ncd".into());
        assert_eq!(
            line(&entry, &TimeZone::UTC),
            "2026-10-01 15:40 parse - Stop_spoof ab_cd missing session_meta"
        );
    }

    #[test]
    fn prints_a_dash_for_null() {
        assert_eq!(
            line(&entry(), &TimeZone::UTC),
            "2026-10-01 15:40 parse - - - missing session_meta"
        );
    }
}
