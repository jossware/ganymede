//! End-to-end tests: the real binary, a payload on stdin, and HOME and
//! XDG_STATE_HOME in a temp dir.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_ganymede");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(rel: &str) -> PathBuf {
    Path::new(FIXTURES).join(rel)
}

struct Sandbox {
    tmp: TempDir,
}

struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl From<Output> for Run {
    fn from(out: Output) -> Run {
        Run {
            status: out.status,
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("home")).unwrap();
        fs::create_dir(tmp.path().join("vault")).unwrap();
        Sandbox { tmp }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn vault(&self) -> PathBuf {
        self.tmp.path().join("vault")
    }

    fn log(&self) -> PathBuf {
        self.tmp.path().join("state/ganymede/errors.jsonl")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env_clear()
            .env("HOME", self.home())
            .env("XDG_STATE_HOME", self.tmp.path().join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn spawn(&self, mut cmd: Command, stdin: &str) -> Child {
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child
    }

    fn run(&self, cmd: Command, stdin: &str) -> Run {
        self.spawn(cmd, stdin).wait_with_output().unwrap().into()
    }

    /// `hook --dir <vault>` plus `args`, with `stdin` as the payload.
    fn hook(&self, args: &[&str], stdin: &str) -> Run {
        let vault = self.vault();
        let mut all = vec!["hook", "--dir", vault.to_str().unwrap()];
        all.extend(args);
        self.run(self.command(&all), stdin)
    }

    fn entries(&self) -> Vec<Value> {
        match fs::read_to_string(self.log()) {
            Ok(text) => text
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn notes(&self) -> Vec<PathBuf> {
        notes_under(&self.vault())
    }
}

fn notes_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(read) = fs::read_dir(dir) else {
        return found;
    };
    for entry in read {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(notes_under(&path));
        } else if path.extension().is_some_and(|e| e == "md") {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn payload(session: &str, transcript: &Path) -> String {
    json!({
        "session_id": session,
        "transcript_path": transcript,
        "cwd": "/Users/user/Workspaces/sample",
        "hook_event_name": "Stop",
    })
    .to_string()
}

/// stdout holds exactly one JSON object, with only a `systemMessage`.
fn assert_one_alert(stdout: &str) -> String {
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout: {stdout:?}");
    let value: Value = serde_json::from_str(lines[0]).unwrap();
    let object = value.as_object().unwrap();
    assert_eq!(object.len(), 1, "stdout: {stdout:?}");
    object["systemMessage"].as_str().unwrap().to_string()
}

const CLAUDE_ID: &str = "5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e";
const CODEX_ID: &str = "01a0b000-0000-7000-8000-000000000001";

#[test]
fn hook_archives_a_claude_session() {
    let sb = Sandbox::new();
    let run = sb.hook(&[], &payload(CLAUDE_ID, &fixture("claude/basic.jsonl")));
    assert!(run.status.success());
    assert_eq!(run.stdout, "");
    let notes = sb.notes();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].starts_with(sb.vault().join("Claude/sample")));
    assert_eq!(
        run.stderr,
        format!("ganymede: archived {}\n", notes[0].display())
    );
    assert!(sb.entries().is_empty());
}

#[test]
fn hook_archives_a_codex_session() {
    let sb = Sandbox::new();
    let run = sb.hook(&[], &payload(CODEX_ID, &fixture("codex/tui.jsonl")));
    assert!(run.status.success());
    assert_eq!(run.stdout, "");
    let notes = sb.notes();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].starts_with(sb.vault().join("Codex")));
}

#[test]
fn hook_rewrites_the_same_note() {
    let sb = Sandbox::new();
    let input = payload(CLAUDE_ID, &fixture("claude/basic.jsonl"));
    sb.hook(&[], &input);
    let first = sb.notes();
    let run = sb.hook(&[], &input);
    assert!(run.status.success());
    assert_eq!(sb.notes(), first);
}

#[test]
fn hook_is_silent_on_an_empty_session() {
    let sb = Sandbox::new();
    let run = sb.hook(
        &[],
        &payload(CLAUDE_ID, &fixture("claude/snapshot_only.jsonl")),
    );
    assert!(run.status.success());
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "");
    assert!(sb.notes().is_empty());
    assert!(sb.entries().is_empty());
}

#[test]
fn hook_reports_a_skipped_session_on_stderr_only() {
    let sb = Sandbox::new();
    let run = sb.hook(&[], &payload(CODEX_ID, &fixture("codex/exec.jsonl")));
    assert!(run.status.success());
    assert_eq!(run.stdout, "");
    assert!(run.stderr.starts_with("skipped: "), "{:?}", run.stderr);
    assert!(sb.notes().is_empty());
    assert!(sb.entries().is_empty());
}

#[test]
fn hook_alerts_once_per_session_and_stage() {
    let sb = Sandbox::new();
    let input = payload(CODEX_ID, &fixture("codex/tui.jsonl"));
    let first = sb.hook(&["--harness", "claude"], &input);
    assert!(first.status.success());
    let alert = assert_one_alert(&first.stdout);
    assert!(
        alert.starts_with("ganymede: couldn't archive this session (parse: "),
        "{alert}"
    );

    let second = sb.hook(&["--harness", "claude"], &input);
    assert!(second.status.success());
    assert_eq!(second.stdout, "");

    let entries = sb.entries();
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        assert_eq!(entry["stage"], "parse");
        assert_eq!(entry["harness"], "claude");
        assert_eq!(entry["session"], CODEX_ID);
        assert_eq!(entry["event"], "Stop");
    }
}

#[test]
fn hook_logs_the_detected_harness_of_a_parse_failure() {
    let sb = Sandbox::new();
    // An interactive rollout from before item_completed existed.
    let run = sb.hook(&[], &payload(CODEX_ID, &fixture("codex/legacy.jsonl")));
    assert!(run.status.success());
    let entries = sb.entries();
    assert_eq!(entries.len(), 1, "stderr: {}", run.stderr);
    assert_eq!(entries[0]["stage"], "parse");
    assert_eq!(entries[0]["harness"], "codex");
}

#[test]
fn hook_alerts_on_every_bad_payload() {
    let sb = Sandbox::new();
    for _ in 0..2 {
        let run = sb.hook(&[], "not json");
        assert!(run.status.success());
        let alert = assert_one_alert(&run.stdout);
        assert!(alert.contains("(payload: payload is not JSON"), "{alert}");
    }
    assert_eq!(sb.entries().len(), 2);
}

#[test]
fn hook_reports_a_missing_archive_dir() {
    let sb = Sandbox::new();
    let missing = sb.tmp.path().join("no-vault");
    let run = sb.run(
        sb.command(&["hook", "--dir", missing.to_str().unwrap()]),
        &payload(CLAUDE_ID, &fixture("claude/basic.jsonl")),
    );
    assert!(run.status.success());
    let alert = assert_one_alert(&run.stdout);
    assert!(alert.contains("(write: "), "{alert}");
    assert_eq!(sb.entries()[0]["stage"], "write");
}

#[test]
fn hook_requires_an_archive_dir() {
    let sb = Sandbox::new();
    let run = sb.run(
        sb.command(&["hook"]),
        &payload(CLAUDE_ID, &fixture("claude/basic.jsonl")),
    );
    assert!(run.status.success());
    let alert = assert_one_alert(&run.stdout);
    assert!(alert.contains("(payload: --dir is required"), "{alert}");
    assert_eq!(sb.entries()[0]["stage"], "payload");
    assert!(sb.notes().is_empty());
}

#[test]
fn hook_survives_a_bad_flag() {
    let sb = Sandbox::new();
    let input = payload(CLAUDE_ID, &fixture("claude/basic.jsonl"));
    let first = sb.hook(&["--bogus"], &input);
    assert!(first.status.success());
    let alert = assert_one_alert(&first.stdout);
    assert!(
        alert.contains("(payload: error: unexpected argument"),
        "{alert}"
    );
    let second = sb.hook(&["--bogus"], &input);
    assert!(second.status.success());
    assert_eq!(second.stdout, "");
    let entries = sb.entries();
    assert_eq!(entries[0]["stage"], "payload");
    assert_eq!(entries[0]["session"], CLAUDE_ID);
}

#[test]
fn hook_alerts_without_home() {
    let sb = Sandbox::new();
    let mut cmd = sb.command(&["hook"]);
    cmd.env_remove("HOME");
    let run = sb.run(cmd, &payload(CLAUDE_ID, &fixture("claude/basic.jsonl")));
    assert!(run.status.success());
    let alert = assert_one_alert(&run.stdout);
    assert!(alert.contains("(payload: HOME is not set)"), "{alert}");
    assert_eq!(sb.entries()[0]["stage"], "payload");
}

#[cfg(debug_assertions)]
#[test]
fn hook_survives_a_panic() {
    let sb = Sandbox::new();
    let transcript = fixture("claude/basic.jsonl");
    let mut cmd = sb.command(&["hook"]);
    cmd.env("GANYMEDE_TEST_PANIC", "1");
    let run = sb.run(cmd, &payload(CLAUDE_ID, &transcript));
    assert!(run.status.success(), "stderr: {}", run.stderr);
    let alert = assert_one_alert(&run.stdout);
    assert!(
        alert.contains("(panic: GANYMEDE_TEST_PANIC is set at "),
        "{alert}"
    );
    let entries = sb.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["stage"], "panic");
    assert_eq!(entries[0]["session"], CLAUDE_ID);
    assert_eq!(entries[0]["transcript"], transcript.to_str().unwrap());
}

#[test]
fn concurrent_hooks_leave_one_note() {
    let sb = Sandbox::new();
    let input = payload(CLAUDE_ID, &fixture("claude/basic.jsonl"));
    let vault = sb.vault();
    let args = ["hook", "--dir", vault.to_str().unwrap()];
    let children = [
        sb.spawn(sb.command(&args), &input),
        sb.spawn(sb.command(&args), &input),
    ];
    for child in children {
        let run: Run = child.wait_with_output().unwrap().into();
        assert!(run.status.success());
        assert_eq!(run.stdout, "");
    }
    let notes = sb.notes();
    assert_eq!(notes.len(), 1);

    let fresh = Sandbox::new();
    fresh.hook(&[], &input);
    assert_eq!(
        fs::read_to_string(&notes[0]).unwrap(),
        fs::read_to_string(&fresh.notes()[0]).unwrap()
    );
}

#[test]
fn render_prints_the_note() {
    let sb = Sandbox::new();
    let transcript = fixture("claude/basic.jsonl");
    let run = sb.run(sb.command(&["render", transcript.to_str().unwrap()]), "");
    assert!(run.status.success(), "stderr: {}", run.stderr);
    assert!(run.stdout.starts_with("---\n"));
    assert!(run.stdout.contains(&format!("session: {CLAUDE_ID}")));
    assert!(sb.notes().is_empty());
}

#[test]
fn render_writes_into_the_archive() {
    let sb = Sandbox::new();
    let transcript = fixture("claude/basic.jsonl");
    let vault = sb.vault();
    let run = sb.run(
        sb.command(&[
            "render",
            transcript.to_str().unwrap(),
            "--write",
            "--dir",
            vault.to_str().unwrap(),
        ]),
        "",
    );
    assert!(run.status.success(), "stderr: {}", run.stderr);
    assert_eq!(run.stdout, "");
    let notes = sb.notes();
    assert_eq!(notes.len(), 1);
    assert!(
        run.stderr
            .ends_with(&format!("ganymede: archived {}\n", notes[0].display()))
    );
}

#[test]
fn render_write_requires_an_archive_dir() {
    let sb = Sandbox::new();
    let transcript = fixture("claude/basic.jsonl");
    let run = sb.run(
        sb.command(&["render", transcript.to_str().unwrap(), "--write"]),
        "",
    );
    assert!(!run.status.success());
    assert!(run.stderr.contains("--dir is required"), "{}", run.stderr);
    assert!(sb.notes().is_empty());
}

#[test]
fn render_fails_loudly_without_logging() {
    let sb = Sandbox::new();
    let transcript = sb.tmp.path().join("foreign.jsonl");
    fs::write(&transcript, "{\"hello\":1}\n").unwrap();
    let run = sb.run(sb.command(&["render", transcript.to_str().unwrap()]), "");
    assert!(!run.status.success());
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "ganymede: unrecognised transcript format\n");
    assert!(sb.entries().is_empty());
}

#[test]
fn render_lists_unhandled_types() {
    let sb = Sandbox::new();
    let transcript = fixture("claude/unknown.jsonl");
    let run = sb.run(sb.command(&["render", transcript.to_str().unwrap()]), "");
    assert!(run.status.success(), "stderr: {}", run.stderr);
    let lines: Vec<&str> = run.stderr.lines().collect();
    assert_eq!(
        lines,
        [
            "unhandled: claude:block/server_tool_use 1",
            "unhandled: claude:record/hologram 1",
        ]
    );
}

#[test]
fn errors_lists_newest_first() {
    let sb = Sandbox::new();
    sb.hook(&[], "not json");
    sb.hook(
        &["--harness", "claude"],
        &payload(CODEX_ID, &fixture("codex/tui.jsonl")),
    );

    let run = sb.run(sb.command(&["errors"]), "");
    assert!(run.status.success());
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].contains(" parse claude Stop 01a0b000 "),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains(" payload - - - payload is not JSON"),
        "{}",
        lines[1]
    );

    let run = sb.run(sb.command(&["errors", "-n", "1"]), "");
    assert_eq!(run.stdout.lines().count(), 1);
}

#[test]
fn errors_is_quiet_without_a_log() {
    let sb = Sandbox::new();
    let run = sb.run(sb.command(&["errors"]), "");
    assert!(run.status.success());
    assert_eq!(run.stdout, "");
}
