# Device File Operations — Round 2 Review Fixes

Follow-up plan from the second review-loop round on `feature/file-operations`.
Three Codex reviewers (one fix-diff path, two full-review paths) surfaced 30+
findings. This plan tracks every issue we accepted as a real problem; see
the sibling `.tasks.json` for the authoritative progress state.

## Tracker → docs/superpowers/plans/2026-04-12-file-ops-review-round2-fixes.md.tasks.json

## Real-fix scope

We are NOT skipping anything that is a true correctness, security,
resource-safety, or spec-adherence problem. The five items still classified
as deferred are:

- `move_result` proto field name vs spec's `move` — Rust keyword conflict, no
  clean fix
- POSIX-style copy/move with destination-as-directory semantics — design
  decision; current code documents "exact path only"
- Cross-filesystem (EXDEV) move test — needs multi-FS test infra; deferred
  with a tracking note
- Hub HTTP body in JSON vs protobuf — Round 1 decided application/x-protobuf;
  this is a contract decision, not a bug
- `T17` 30s timeout integration test — production constant; would need a
  test-only override hook

Everything else gets fixed.
