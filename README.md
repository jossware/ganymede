# ganymede

ganymede archives agent sessions into an Obsidian vault. A harness hook
runs it at the end of each turn, and it writes the session as one
Markdown note with queryable frontmatter.

It supports two harnesses:

- **Claude Code**, from its session transcripts.
- **Codex**, TUI and Desktop, from its rollout files. Only interactive
  sessions are archived, including Desktop quick chats and ChatGPT
  conversations continued in Codex. `codex exec`, review subagents,
  scheduled automations, and voice chats are skipped.

The name comes from the Junior Ganymede, the club whose members record
their employers' doings in the club book.

## Install

ganymede is packaged as a Nix flake:

```sh
nix profile install github:ryane/ganymede
```

Or add it as a flake input and use `packages.<system>.default`:

```nix
inputs.ganymede.url = "github:ryane/ganymede";
```

From a checkout, run `nix build` or `nix profile install .`.

## Hook setup

Run `ganymede hook` on both `Stop` and `SessionEnd`. `Stop` keeps the
note current after every turn. `SessionEnd` catches anything written
after the last turn.

The hook always exits 0. It never blocks or steers the harness. Its
stdout is empty, except for a one-time alert on failure (see
[Errors](#errors)).

### Claude Code

In `~/.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "ganymede hook --thinking=false --tool-output=false" }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "ganymede hook --thinking=false --tool-output=false" }] }
    ]
  }
}
```

### Codex

Hooks MUST be enabled in `~/.codex/config.toml`:

```toml
[features]
hooks = true
```

Then add ganymede to `~/.codex/hooks.json`. Keep any entries already
there.

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "ganymede hook --thinking=false --tool-output=false" }] }
    ],
    "SessionEnd": [
      { "hooks": [{ "type": "command", "command": "ganymede hook --thinking=false --tool-output=false" }] }
    ]
  }
}
```

Codex does not run a new hook until you approve it. Approval is stored
as a hash per entry under `[hooks.state]` in `config.toml`.

1. Start the Codex TUI and approve the new hooks when prompted.
2. Finish one turn. Confirm a note appears under `<dir>/Codex/`.
3. Codex Desktop shares the TUI's approvals. Finish one Desktop turn and
   confirm its note too.

Approve in the TUI first. It is unverified whether Desktop prompts for
an untrusted hook or skips it silently.

## Commands

### `ganymede hook`

Archives the session named by a hook payload on stdin. It reads
`transcript_path` from the payload. Both harnesses send the same shape.
A null `transcript_path` means the session was never persisted, and
the hook does nothing.

| Flag | Default | |
|---|---|---|
| `--dir <DIR>` | `~/org/Calendar/AI` | Archive root. MUST already exist. |
| `--harness <claude\|codex>` | detected | Transcript format. |
| `--thinking <true\|false>` | `true` | Include thinking blocks. |
| `--tool-output <true\|false>` | `true` | Include tool output under each tool call. |
| `--max-lines <N>` | `100` | Truncate each tool output to N lines. |

Boolean flags take a value: `--thinking=false`, not `--no-thinking`.

On success it prints `ganymede: archived <path>` to stderr.

### `ganymede render <transcript>`

Renders a transcript to stdout. It takes the same flags as `hook`, plus:

| Flag | |
|---|---|
| `--write` | Write into the archive, exactly as `hook` would, instead of printing. |
| `--force` | Archive a Codex session the interactivity filter would skip. |

Unknown content types are listed on stderr as `unhandled: <type>
<count>`. Unlike `hook`, `render` exits non-zero on failure.

Use it to preview a note, or to backfill sessions the hook missed:

```sh
for f in ~/.codex/sessions/2026/09/30/rollout-*.jsonl; do
  ganymede render --write "$f"
done
```

Archiving a Codex Desktop conversation moves its rollout to
`~/.codex/archived_sessions/`. Look there when a stored transcript path
no longer exists.

### `ganymede errors`

Prints recent error-log entries, newest first, one per line:

```
<date> <time> <stage> <harness> <event> <session8> <error>
```

`-n <count>` limits the output. The default is 20.

## The archive

Notes live at:

```
<dir>/<Claude|Codex>/<project>/<YYYY-MM-DD>-<title-slug>-<id8>.md
```

- `project` is the repository's name, read from `.git` or `.jj` without
  running either tool. A session in a git or jj worktree is filed under
  its main repository's name; the worktree's name goes in the
  `worktree` field. Outside any repository, `project` is the
  directory's basename. Codex Desktop chats not tied to a folder are
  filed under `chat`.
- The date is the session's start date. It never changes.
- `title-slug` is the title reduced to ASCII `a-z0-9` and hyphens. When
  that leaves nothing, as with a non-Latin or punctuation-only title,
  the segment and its hyphen are dropped: `<YYYY-MM-DD>-<id8>.md`.
- `id8` is the first 8 characters of the session id.

One session is one note. Before writing, ganymede searches the harness
folder for a note whose `session:` matches, and rewrites it in place.
A note never moves once written, even if the session crosses midnight or
changes directory.

Notes are generated output. Every rewrite replaces the whole note, so
hand edits are lost.

A session with no user message produces no note.

### Frontmatter

```yaml
---
type: session
title: "Add greeting to sample CLI"
date: 2026-09-03
journal: "[[2026-09-03]]"
updated: 2026-09-03T22:25
days: ["[[2026-09-03]]"]
project: sample
harness: claude
harness_version: 2.1.259
model: claude-fable-5-1
session: 5f0c2a1e-7b3d-4c9a-9e21-3d4b5a6c7d8e
cc_version: v2.1.259
worktree: feature
transcript: /Users/me/.claude/projects/-Users-me-sample/5f0c2a1e-….jsonl
unhandled: ["claude:record/hologram"]
---
```

| Field | |
|---|---|
| `title` | Claude Code's own session title if it set one, else derived from the first user message. |
| `date`, `journal` | Start date. |
| `updated` | Time of the last entry, local, to the minute. |
| `days` | Start date and every local date with an entry. |
| `cc_version` | Claude Code only. Kept for compatibility with older notes. |
| `worktree` | Only when the session ran in a secondary checkout. |
| `unhandled` | Only when the transcript held types ganymede does not know. |

Optional fields are omitted, never written empty.

### Body

User messages render as `[!NOTE]` callouts. Each run of agent activity
gets one timestamped header, followed by its prose, tool calls, and
(with `--thinking`) thinking. Subagent reports render as `[!TIP]`
callouts. A failed tool call is marked `(failed)`.

Text is escaped so Obsidian shows it as written. Bare `<` and `>`,
`#tags`, `%%` comments, and setext underlines are neutralized outside
code.

## Errors

A hook that fails still exits 0, so failures go to a log instead:

```
$XDG_STATE_HOME/ganymede/errors.jsonl
~/.local/state/ganymede/errors.jsonl    # when XDG_STATE_HOME is unset
```

Each line is one JSON object:

```json
{"ts":"…","version":"…","harness":"codex","event":"Stop","session":"…","transcript":"…","stage":"parse","error":"…"}
```

`stage` is one of `payload`, `detect`, `parse`, `render`, `write`, or
`panic`. The log is created `0600`, since entries carry session ids and
paths. It is kept under 1 MiB by dropping its older half.

The first failure for a session at a given stage also alerts the
harness:

```json
{"systemMessage":"ganymede: couldn't archive this session (parse: …). Details: ~/.local/state/ganymede/errors.jsonl"}
```

Later failures at the same stage are logged silently. Codex Desktop does
not show `systemMessage`; check `ganymede errors` there.

Unknown transcript content is not an error. It is archived where
possible and listed in the note's `unhandled` field, so a vault query
can find notes that need a newer ganymede.

The same state directory holds `archive.lock`. It serializes
overlapping `Stop` and `SessionEnd` runs, so a stale render cannot land
last.

## Development

Work inside the devShell. With direnv, `cd` into the repository. Without
it, prefix commands with `direnv exec .` or use `nix develop`.

Outside the devShell, linking fails on macOS with `ld: library not found
for -liconv`.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs the same checks, then `nix build`.

Renderer tests use [insta](https://insta.rs) snapshots. Review changes
with `cargo insta review`.

Adapter fixtures are synthetic transcripts in `tests/fixtures/`. See
[its README](tests/fixtures/README.md) to regenerate them.

## License

[MIT](LICENSE)
