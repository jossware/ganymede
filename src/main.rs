mod cli;
// Unused until the renderer and archive consume the model.
#[allow(dead_code)]
mod harness;
#[allow(dead_code)]
mod project;
#[allow(dead_code)]
mod render;
#[allow(dead_code)]
mod session;
#[allow(dead_code)]
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
