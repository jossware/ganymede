mod cli;

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
