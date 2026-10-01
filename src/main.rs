mod archive;
mod cli;
// Unused until the CLI records failures.
#[allow(dead_code)]
mod errlog;
// Unused until the CLI loads transcripts.
#[allow(dead_code)]
mod harness;
// Unused until the CLI saves sessions or records failures.
#[allow(dead_code)]
mod lock;
// Unused until the commands are wired.
#[allow(dead_code)]
mod paths;
#[allow(dead_code)]
mod pipeline;
mod project;
mod render;
#[allow(dead_code)]
mod session;
mod title;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        // A hook MUST NOT fail the harness.
        Command::Hook(_) => ExitCode::SUCCESS,
        Command::Render { .. } | Command::Errors { .. } => {
            eprintln!("ganymede: not implemented");
            ExitCode::FAILURE
        }
    }
}
