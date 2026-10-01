use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};
use jiff::tz::TimeZone;

use crate::harness::Harness;
use crate::render;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Archive the session named by a hook payload on stdin.
    Hook(RenderOpts),
    /// Render a transcript to stdout, or into the archive with --write.
    Render {
        transcript: PathBuf,
        #[command(flatten)]
        opts: RenderOpts,
        /// Write into the archive instead of printing.
        #[arg(long)]
        write: bool,
        /// Archive Codex sessions the interactivity filter would skip.
        #[arg(long)]
        force: bool,
    },
    /// Print recent error-log entries, newest first.
    Errors {
        #[arg(short = 'n', default_value_t = 20)]
        count: usize,
    },
}

#[derive(Debug, Args)]
pub struct RenderOpts {
    /// Archive root [default: ~/org/Calendar/AI]
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Transcript format; detected when omitted.
    #[arg(long, value_enum)]
    pub harness: Option<Harness>,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub thinking: bool,
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub tool_output: bool,
    #[arg(long, default_value_t = 100)]
    pub max_lines: usize,
}

impl RenderOpts {
    pub fn render_options(&self) -> render::Options {
        render::Options {
            thinking: self.thinking,
            tool_output: self.tool_output,
            max_lines: self.max_lines,
            tz: TimeZone::system(),
        }
    }
}

/// Whether the subcommand is `hook`, judged before clap parses, since
/// `hook` MUST exit 0 even when its arguments are bad.
pub fn is_hook(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .find(|a| !a.to_string_lossy().starts_with('-'))
        .is_some_and(|a| a == "hook")
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn hook_accepts_dotfiles_flags() {
        let cli = Cli::try_parse_from([
            "ganymede",
            "hook",
            "--dir",
            "/tmp/ai",
            "--thinking=false",
            "--tool-output=false",
        ])
        .unwrap();
        let Command::Hook(opts) = cli.command else {
            panic!("expected hook");
        };
        assert!(!opts.thinking);
        assert!(!opts.tool_output);
        assert_eq!(opts.max_lines, 100);
    }

    #[test]
    fn spots_hook_before_parsing() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(is_hook(&args(&["ganymede", "hook", "--bogus"])));
        assert!(is_hook(&args(&["ganymede", "--bogus", "hook"])));
        assert!(!is_hook(&args(&["ganymede", "render", "hook"])));
        assert!(!is_hook(&args(&["ganymede"])));
    }
}
