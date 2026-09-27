# Fixtures

Transcripts for adapter tests. Record envelopes, field names, and key
order are copied from real local Claude Code (2.1.259) and Codex
(0.155–0.157) sessions. All content, paths, and ids are synthetic.

Regenerate after editing a generator:

```sh
python3 tests/fixtures/gen/gen_claude.py tests/fixtures/claude
python3 tests/fixtures/gen/gen_codex.py tests/fixtures/codex
```

Each fixture's purpose is the comment above it in its generator.

Not seen locally, so written from cc2md's tests instead of real records:
`<teammate-message>`, `<local-command-stderr>`, `redacted_thinking`, and
a `document` block with a `title`.
