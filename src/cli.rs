use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand};
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
/// `hook` MUST exit 0 even when its arguments are bad. Options misplaced
/// before the subcommand are stepped over with their values, so a value
/// such as `--dir render` is not read as the subcommand.
pub fn is_hook(args: &[OsString]) -> bool {
    let cli = Cli::command();
    let takes_value = |flag: &str| {
        cli.get_subcommands()
            .flat_map(|sub| sub.get_arguments())
            .filter(|arg| arg.get_action().takes_values())
            .any(|arg| {
                arg.get_long()
                    .is_some_and(|l| flag.strip_prefix("--") == Some(l))
                    || arg.get_short().is_some_and(|c| flag == format!("-{c}"))
            })
    };
    let mut rest = args.iter().skip(1).map(|a| a.to_string_lossy());
    while let Some(arg) = rest.next() {
        if !arg.starts_with('-') {
            return arg == "hook";
        }
        // `--dir hook` is either a value or a missing value before the
        // subcommand. Read it as the subcommand: failing the harness costs
        // more than mistaking a malformed manual run for a hook.
        if !arg.contains('=') && takes_value(&arg) && rest.next().is_some_and(|v| v == "hook") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
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
        assert!(is_hook(&args(&["ganymede", "--dir", "/x", "hook"])));
        assert!(is_hook(&args(&["ganymede", "-n", "3", "hook"])));
        assert!(!is_hook(&args(&["ganymede", "errors", "-n", "hook"])));
        assert!(is_hook(&args(&["ganymede", "--dir", "render", "hook"])));
        assert!(!is_hook(&args(&[
            "ganymede", "--dir", "/tmp", "errors", "hook"
        ])));
        assert!(is_hook(&args(&["ganymede", "--dir=/x", "hook"])));
        assert!(is_hook(&args(&["ganymede", "--thinking", "false", "hook"])));
        assert!(is_hook(&args(&["ganymede", "--dir", "hook"])));
        assert!(is_hook(&args(&["ganymede", "--thinking", "hook"])));
    }
}
