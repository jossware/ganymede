//! `ganymede render`: a transcript to stdout, or into the archive.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use crate::cli::RenderOpts;
use crate::harness::{self, Loaded};
use crate::paths::{self, Env};
use crate::pipeline::{self, Outcome, Request};
use crate::project;
use crate::render;
use crate::session::Session;

pub struct Args<'a> {
    pub transcript: &'a Path,
    pub opts: &'a RenderOpts,
    pub write: bool,
    pub force: bool,
}

pub fn run(args: &Args, env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    match render(args, env, out, err) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(err, "ganymede: {e}");
            ExitCode::FAILURE
        }
    }
}

fn render(
    args: &Args,
    env: &Env,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(home), Some(state)) = (&env.home, &env.state) else {
        return Err("HOME is not set".into());
    };
    let opts = args.opts.render_options();
    if args.write {
        let Some(dir) = &args.opts.dir else {
            return Err(paths::DIR_REQUIRED.into());
        };
        let lock = paths::archive_lock(state);
        let dir = paths::archive_dir(dir, home);
        let req = Request {
            lock: &lock,
            dir: &dir,
            transcript: args.transcript,
            harness: args.opts.harness,
            force: args.force,
            opts: &opts,
            home,
        };
        match pipeline::archive(&req)? {
            Outcome::Saved { path, session } => {
                unhandled(&session, err)?;
                writeln!(err, "ganymede: archived {}", path.display())?;
            }
            Outcome::Empty => {}
            Outcome::Skipped(reason) => writeln!(err, "skipped: {reason}")?,
        }
        return Ok(());
    }
    match harness::load(args.transcript, args.opts.harness, args.force)? {
        Loaded::Session(session) => {
            unhandled(&session, err)?;
            let id = project::identify(&session.cwds, None, home);
            out.write_all(
                render::note(&session, &id.name, id.worktree.as_deref(), &opts).as_bytes(),
            )?;
        }
        Loaded::Empty => {}
        Loaded::Skipped(reason) => writeln!(err, "skipped: {reason}")?,
    }
    Ok(())
}

fn unhandled(session: &Session, err: &mut dyn Write) -> io::Result<()> {
    for (key, count) in &session.unhandled {
        writeln!(err, "unhandled: {key} {count}")?;
    }
    Ok(())
}
