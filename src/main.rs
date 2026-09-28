mod cli;
// Unused until the renderer and archive consume the model.
#[allow(dead_code)]
mod harness;
#[allow(dead_code)]
mod session;
#[allow(dead_code)]
mod title;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let _ = vec![1].len() == 0;
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
