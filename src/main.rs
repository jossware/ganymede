mod archive;
mod cli;
mod cmd;
mod errlog;
mod harness;
mod lock;
mod paths;
mod pipeline;
mod project;
mod render;
mod session;
mod title;

use std::env;
use std::io;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

use cli::{Cli, Command};
use paths::Env;

fn main() -> ExitCode {
    let env = Env::read();
    let args: Vec<_> = env::args_os().collect();
    let hook = cli::is_hook(&args);
    if hook {
        cmd::hook::install_panic_hook(env.clone());
    }
    let (mut out, mut err) = (io::stdout(), io::stderr());
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e)
            if hook && !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) =>
        {
            let message = e.to_string();
            let first = message.lines().next().unwrap_or("bad arguments");
            cmd::hook::bad_args(first, &env, &mut io::stdin(), &mut out, &mut err);
            return ExitCode::SUCCESS;
        }
        Err(e) => e.exit(),
    };
    match cli.command {
        Command::Hook(opts) => {
            cmd::hook::run(&opts, &env, &mut io::stdin(), &mut out, &mut err);
            ExitCode::SUCCESS
        }
        Command::Render {
            transcript,
            opts,
            write,
            force,
        } => {
            let args = cmd::render::Args {
                transcript: &transcript,
                opts: &opts,
                write,
                force,
            };
            cmd::render::run(&args, &env, &mut out, &mut err)
        }
        Command::Errors { count } => cmd::errors::run(count, &env, &mut out, &mut err),
    }
}
