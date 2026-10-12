//! `ganymede hook`: archive the session a harness hook names on stdin.
//!
//! A hook MUST NOT disturb the harness. Every path exits 0, and stdout
//! carries nothing but one `systemMessage` object when a failure alerts.
//! Stream writes ignore errors: `println!` panics on a closed stream.

use std::fs;
use std::io::{self, Read, Write};
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Mutex, TryLockError};

use serde_json::Value;

use crate::cli::RenderOpts;
use crate::errlog::{self, Alert, Entry, Stage};
use crate::harness::{self, Detected, LoadError, Records};
use crate::paths::{self, Env};
use crate::pipeline::{self, Outcome, Request};

/// Bound on a payload field as logged. PATH_MAX is 1024 on macOS and
/// 4096 on Linux, and session ids are far shorter.
const MAX_FIELD: usize = 4096;

/// What the error log should say about the run so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Context {
    pub harness: Option<String>,
    pub event: Option<String>,
    pub session: Option<String>,
    pub transcript: Option<String>,
}

impl Context {
    const EMPTY: Context = Context {
        harness: None,
        event: None,
        session: None,
        transcript: None,
    };

    fn entry(&self, stage: Stage, error: String) -> Entry {
        let mut entry = Entry::new(stage, error);
        entry.harness = self.harness.clone();
        entry.event = self.event.clone();
        entry.session = self.session.clone();
        entry.transcript = self.transcript.clone();
        entry
    }
}

/// The panic hook's only view of the run.
static CONTEXT: Mutex<Context> = Mutex::new(Context::EMPTY);

fn publish(ctx: &Context) {
    *CONTEXT.lock().unwrap_or_else(|e| e.into_inner()) = ctx.clone();
}

/// The payload's fields, and its transcript or why it is unusable. The
/// fields are filled even from an unusable payload, so the failure still
/// dedupes by session.
///
/// A null `transcript_path` means the session has no transcript: Codex
/// sends one for a thread it never persisted. That is nothing to archive,
/// not a failure.
fn read_payload(input: &[u8]) -> (Context, Result<Option<PathBuf>, String>) {
    let value = serde_json::from_slice::<Value>(input);
    let object = value.as_ref().ok().and_then(Value::as_object);
    let text = |key: &str| {
        object
            .and_then(|o| o.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    // The log is capped, and a malformed payload must not fill it.
    let logged = |key: &str| text(key).map(|s| errlog::clip(s, MAX_FIELD).into_owned());
    let ctx = Context {
        harness: None,
        event: logged("hook_event_name"),
        session: logged("session_id"),
        transcript: logged("transcript_path"),
    };
    let transcript = match (&value, object, text("transcript_path")) {
        (Err(e), _, _) => Err(format!("payload is not JSON: {e}")),
        (Ok(_), None, _) => Err("payload is not a JSON object".to_string()),
        (Ok(_), Some(_), Some(t)) => Ok(Some(PathBuf::from(t))),
        (Ok(_), Some(o), None) if o.get("transcript_path") == Some(&Value::Null) => Ok(None),
        (Ok(_), Some(_), None) => Err("payload has no transcript_path".to_string()),
    };
    (ctx, transcript)
}

pub fn run(
    opts: &RenderOpts,
    env: &Env,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let mut input = Vec::new();
    let read = stdin.read_to_end(&mut input);
    let (mut ctx, transcript) = read_payload(&input);
    ctx.harness = opts.harness.map(|h| h.id().to_string());
    publish(&ctx);

    #[cfg(debug_assertions)]
    if std::env::var_os("GANYMEDE_TEST_PANIC").is_some() {
        panic!("GANYMEDE_TEST_PANIC is set");
    }

    let transcript = match (read, transcript) {
        (Err(e), _) => {
            return fail(
                env,
                &ctx,
                Stage::Payload,
                format!("couldn't read stdin: {e}"),
                out,
                err,
            );
        }
        (Ok(_), Err(e)) => return fail(env, &ctx, Stage::Payload, e, out, err),
        (Ok(_), Ok(None)) => return,
        (Ok(_), Ok(Some(transcript))) => transcript,
    };
    let (Some(home), Some(state)) = (&env.home, &env.state) else {
        return fail(
            env,
            &ctx,
            Stage::Payload,
            "HOME is not set".to_string(),
            out,
            err,
        );
    };

    let Some(dir) = &opts.dir else {
        return fail(
            env,
            &ctx,
            Stage::Payload,
            paths::DIR_REQUIRED.to_string(),
            out,
            err,
        );
    };

    let render_opts = opts.render_options();
    let lock = paths::archive_lock(state);
    let dir = paths::archive_dir(dir, home);
    let req = Request {
        lock: &lock,
        dir: &dir,
        transcript: &transcript,
        harness: opts.harness,
        force: false,
        opts: &render_opts,
        home,
    };
    match pipeline::archive(&req) {
        Ok(Outcome::Saved { path, .. }) => {
            let _ = writeln!(err, "ganymede: archived {}", path.display());
        }
        Ok(Outcome::Empty) => {}
        Ok(Outcome::Skipped(reason)) => {
            let _ = writeln!(err, "skipped: {reason}");
        }
        Err(e) => {
            if ctx.harness.is_none() {
                ctx.harness = detected(&transcript);
            }
            // An I/O error alone does not say which file, and a Codex
            // Desktop rollout can move away once its conversation is archived.
            let error = match &e {
                pipeline::Error::Load(LoadError::Io(_)) => {
                    format!("{}: {e}", transcript.display())
                }
                _ => e.to_string(),
            };
            fail(env, &ctx, stage_of(&e), error, out, err);
        }
    }
}

/// `hook` with arguments clap rejected. The dotfiles own the flags, so a
/// bad one is a payload failure, not a reason to fail the harness.
pub fn bad_args(
    message: &str,
    env: &Env,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let mut input = Vec::new();
    let _ = stdin.read_to_end(&mut input);
    let (ctx, _) = read_payload(&input);
    publish(&ctx);
    fail(env, &ctx, Stage::Payload, message.to_string(), out, err);
}

/// Logs a panic as a `panic` failure, then exits 0. Nothing in here may
/// panic: std aborts on a panic raised inside a panic hook.
pub fn install_panic_hook(env: Env) {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        default(info);
        let ctx = match CONTEXT.try_lock() {
            Ok(ctx) => ctx.clone(),
            Err(TryLockError::Poisoned(e)) => e.into_inner().clone(),
            Err(TryLockError::WouldBlock) => Context::default(),
        };
        let mut out = io::stdout();
        fail(
            &env,
            &ctx,
            Stage::Panic,
            panic_message(info),
            &mut out,
            &mut io::stderr(),
        );
        let _ = out.flush();
        process::exit(0);
    }));
}

fn panic_message(info: &PanicHookInfo) -> String {
    let message = info.payload_as_str().unwrap_or("panic");
    match info.location() {
        Some(at) => format!("{message} at {}:{}", at.file(), at.line()),
        None => message.to_string(),
    }
}

fn stage_of(e: &pipeline::Error) -> Stage {
    match e {
        pipeline::Error::Load(LoadError::Undetected) => Stage::Detect,
        pipeline::Error::Load(_) => Stage::Parse,
        pipeline::Error::Save(_) => Stage::Write,
    }
}

/// The transcript's harness, for an entry whose load failed.
fn detected(transcript: &Path) -> Option<String> {
    let text = fs::read_to_string(transcript).ok()?;
    match harness::detect(&Records::parse(&text)) {
        Detected::Harness(h) => Some(h.id().to_string()),
        Detected::Empty | Detected::Unknown => None,
    }
}

/// Logs the failure and prints any alert.
fn fail(
    env: &Env,
    ctx: &Context,
    stage: Stage,
    error: String,
    out: &mut dyn Write,
    err: &mut dyn Write,
) {
    let _ = writeln!(
        err,
        "ganymede: {}: {}",
        stage.as_str(),
        errlog::one_line(&error)
    );
    let alert = match &env.state {
        Some(state) => {
            let home = env.home.as_deref().unwrap_or(Path::new(""));
            errlog::record(&paths::error_log(state), home, &ctx.entry(stage, error))
        }
        // No log to point at, so no `Details:` clause.
        None => Alert::Yes(format!(
            "ganymede: couldn't archive this session ({}: {}).",
            stage.as_str(),
            errlog::brief(&error),
        )),
    };
    if let Alert::Yes(text) = alert {
        let _ = writeln!(out, "{}", serde_json::json!({ "systemMessage": text }));
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn hook_opts(args: &[&str]) -> RenderOpts {
        let cli = Cli::try_parse_from(["ganymede", "hook"].iter().chain(args)).unwrap();
        let Command::Hook(opts) = cli.command else {
            panic!("expected hook");
        };
        opts
    }

    fn context(session: &str, transcript: &str) -> Context {
        Context {
            harness: None,
            event: Some("Stop".into()),
            session: Some(session.into()),
            transcript: Some(transcript.into()),
        }
    }

    #[test]
    fn reads_a_payload() {
        let (ctx, transcript) = read_payload(
            br#"{"session_id":"s1","transcript_path":"/t.jsonl","hook_event_name":"Stop","cwd":"/x"}"#,
        );
        assert_eq!(ctx, context("s1", "/t.jsonl"));
        assert_eq!(transcript, Ok(Some(PathBuf::from("/t.jsonl"))));
    }

    #[test]
    fn null_transcript_path_is_no_transcript() {
        let (ctx, transcript) = read_payload(
            br#"{"session_id":"s1","transcript_path":null,"hook_event_name":"SessionEnd"}"#,
        );
        assert_eq!(ctx.session.as_deref(), Some("s1"));
        assert_eq!(transcript, Ok(None));
    }

    #[test]
    fn rejects_unusable_payloads() {
        for (input, why) in [
            (&b"not json"[..], "payload is not JSON"),
            (b"[1, 2]", "payload is not a JSON object"),
            (b"{}", "payload has no transcript_path"),
            (
                br#"{"transcript_path":""}"#,
                "payload has no transcript_path",
            ),
            (
                br#"{"transcript_path":7}"#,
                "payload has no transcript_path",
            ),
        ] {
            let (_, transcript) = read_payload(input);
            let e = transcript.unwrap_err();
            assert!(e.starts_with(why), "{e:?} for {input:?}");
        }
    }

    #[test]
    fn bounds_payload_fields_for_the_log() {
        let huge = "x".repeat(1 << 20);
        let input = serde_json::json!({
            "session_id": huge,
            "hook_event_name": huge,
            "transcript_path": huge,
        })
        .to_string();
        let (ctx, transcript) = read_payload(input.as_bytes());
        for field in [&ctx.session, &ctx.event, &ctx.transcript] {
            let len = field.as_ref().unwrap().len();
            assert!(len < 2 * MAX_FIELD, "{len} bytes");
        }
        assert_eq!(transcript.unwrap().unwrap().as_os_str().len(), huge.len());
    }

    #[test]
    fn keeps_the_session_of_an_unusable_payload() {
        let (ctx, transcript) = read_payload(br#"{"session_id":"s1"}"#);
        assert_eq!(ctx.session.as_deref(), Some("s1"));
        assert!(transcript.is_err());
    }

    #[test]
    fn maps_failures_to_stages() {
        let load = |e| stage_of(&pipeline::Error::Load(e));
        assert_eq!(load(LoadError::Undetected), Stage::Detect);
        assert_eq!(load(LoadError::Parse("x".into())), Stage::Parse);
        assert_eq!(
            load(LoadError::Io(io::ErrorKind::NotFound.into())),
            Stage::Parse
        );
        let save = pipeline::Error::Save(crate::archive::Error::Locked);
        assert_eq!(stage_of(&save), Stage::Write);
    }

    #[test]
    fn entry_carries_the_context() {
        let entry = context("s1", "/t.jsonl").entry(Stage::Parse, "boom".into());
        assert_eq!(entry.event.as_deref(), Some("Stop"));
        assert_eq!(entry.session.as_deref(), Some("s1"));
        assert_eq!(entry.transcript.as_deref(), Some("/t.jsonl"));
        assert_eq!(entry.harness, None);
    }

    #[test]
    fn alerts_without_a_state_dir() {
        let env = Env {
            home: None,
            state: None,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        fail(
            &env,
            &Context::default(),
            Stage::Payload,
            "HOME is not set".into(),
            &mut out,
            &mut err,
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"systemMessage\":\"ganymede: couldn't archive this session (payload: HOME is not set).\"}\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "ganymede: payload: HOME is not set\n"
        );
    }

    #[test]
    fn clips_a_long_error_without_a_state_dir() {
        let env = Env {
            home: None,
            state: None,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let error = "word ".repeat(500);
        fail(
            &env,
            &Context::default(),
            Stage::Parse,
            error,
            &mut out,
            &mut err,
        );
        let alert: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let text = alert["systemMessage"].as_str().unwrap();
        assert!(text.len() < 400, "{} bytes", text.len());
    }

    #[test]
    fn run_logs_and_alerts_once_per_session() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            home: Some(tmp.path().to_path_buf()),
            state: Some(tmp.path().join("state")),
        };
        let payload = format!(
            r#"{{"session_id":"s1","transcript_path":"{}","hook_event_name":"Stop"}}"#,
            tmp.path().join("missing.jsonl").display()
        );
        let opts = hook_opts(&["--dir", &tmp.path().display().to_string()]);
        let mut stdouts = Vec::new();
        for _ in 0..2 {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            run(&opts, &env, &mut payload.as_bytes(), &mut out, &mut err);
            stdouts.push(String::from_utf8(out).unwrap());
        }
        assert!(
            stdouts[0].starts_with(
                "{\"systemMessage\":\"ganymede: couldn't archive this session (parse: "
            )
        );
        assert_eq!(stdouts[1], "");
        let log = paths::error_log(env.state.as_ref().unwrap());
        let entries = errlog::recent(&log, 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session.as_deref(), Some("s1"));
    }

    #[test]
    fn run_ignores_a_session_without_a_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            home: Some(tmp.path().to_path_buf()),
            state: Some(tmp.path().join("state")),
        };
        let payload =
            r#"{"session_id":"s1","transcript_path":null,"hook_event_name":"SessionEnd"}"#;
        let opts = hook_opts(&["--dir", &tmp.path().display().to_string()]);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(&opts, &env, &mut payload.as_bytes(), &mut out, &mut err);
        assert!(out.is_empty() && err.is_empty());
        assert!(!paths::error_log(env.state.as_ref().unwrap()).exists());
    }

    #[test]
    fn names_a_missing_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            home: Some(tmp.path().to_path_buf()),
            state: Some(tmp.path().join("state")),
        };
        let missing = tmp.path().join("missing.jsonl");
        let payload = format!(r#"{{"transcript_path":"{}"}}"#, missing.display());
        let opts = hook_opts(&["--dir", &tmp.path().display().to_string()]);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run(&opts, &env, &mut payload.as_bytes(), &mut out, &mut err);
        let log = paths::error_log(env.state.as_ref().unwrap());
        let error = &errlog::recent(&log, 1).unwrap()[0].error;
        assert!(
            error.starts_with(&format!("{}: ", missing.display())),
            "{error}"
        );
    }
}
