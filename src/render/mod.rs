pub mod escape;
pub mod frontmatter;
pub mod markdown;

use jiff::tz::TimeZone;

pub struct Options {
    pub thinking: bool,
    pub tool_output: bool,
    /// 0 means no limit.
    pub max_lines: usize,
    /// Zone for every date and time in the note.
    pub tz: TimeZone,
}
