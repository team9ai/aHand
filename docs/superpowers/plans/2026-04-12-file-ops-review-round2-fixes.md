# Device File Operations — Round 2 Review Fixes

Follow-up plan from the second review-loop round on `feature/file-operations`.
Three Codex reviewers (one fix-diff path, two full-review paths) surfaced 30+
findings. This plan tracks every issue we accepted as a real problem; see
the sibling `.tasks.json` for the authoritative progress state.

> **Round 3 followed.** After Round 2 a third review-loop round ran
> directly inline; its fixes (C1 connection-teardown deadlock, C2
> relative-symlink dispatch, C3 glob fail-closed at scan cap, C4 deep-list
> offset rejection, C5 TRASH recursive guard, plus I1 Hub pending-slot
> cancel cleanup, I2 fail-loud tilde expansion, I3 stat-then-refuse on
> oversized edits, I4 structured `ApprovalEscalation` reason, I6 portable
> EXDEV detection, plus follow-ups from spec-quality / test-completeness /
> Copilot / Codex passes) landed commit-by-commit on this branch and are
> not tracked as a separate plan. See the PR description and commit log
> for the round-by-round breakdown.

## Tracker → docs/superpowers/plans/2026-04-12-file-ops-review-round2-fixes.md.tasks.json

## Real-fix scope

We are NOT skipping anything that is a true correctness, security,
resource-safety, or spec-adherence problem. The four items still classified
as deferred are:

- **`move_result` proto field name vs spec's `move`** — Rust keyword conflict.
  Proto uses `move_result`; the spec text was updated in the doc-consistency
  pass to match.
- **POSIX-style copy/move with destination-as-directory semantics** —
  design decision. Current code uses exact-path semantics (no auto-treat-as-dir).
  Spec / `crates/ahandd/src/file_manager/fs_ops.rs` document this explicitly.
- **Cross-filesystem (EXDEV) move test** — needs multi-FS test infra;
  the helper `cross_device_move_fallback` is now extracted with its own
  unit tests (see Round 3 follow-up), and `is_cross_device_error` is
  portably tested via `io::ErrorKind::CrossesDevices`. The end-to-end
  trigger from real cross-FS rename remains deferred.
- **Hub HTTP body in JSON vs protobuf** — Round 1 decided
  `application/x-protobuf`; this is a contract decision, not a bug.
- **R10 / TOCTOU residual — full openat2 / RESOLVE_NO_SYMLINKS fix**
  remains open. The Move data-loss vector that this concretely
  produced (rename destroys source → post-canonicalize rejects →
  cleanup deletes the destination → both copies of the data are
  gone) was **closed in round 4 follow-up `fix/file-ops-r10-toctou`**
  by switching `verify_post_create`'s cleanup to a per-call-site
  policy: `RemoveFileOrDir` for Mkdir / Write / Symlink, the new
  `RemoveTreeAll` for recursive Copy (so partial trees don't leak
  on rejection), and the new `Leave` for Move (which now preserves
  data at the rejected destination so the operator can recover).
  The race window itself still exists — closing it requires
  `openat2(RESOLVE_NO_SYMLINKS)` on Linux + `O_NOFOLLOW`-chain on
  macOS + Windows equivalent — but the resulting failure no longer
  destroys user data; it surfaces as `PolicyDenied` with the
  rejected path named in the FileError.
- **`T17` 30s timeout integration test** — production constant; would
  need a test-only override hook. Round 3's `sent_job_fails_after_disconnect_grace_without_reconnect`
  CI-stability fix uses an elapsed-time floor instead, which is the
  closest reusable pattern.

(Round 1's "S15 PendingFileRequests placement" deferral was actually
completed as R17 in tasks.json — the refactor that moved the type out
of `http/files.rs` into its own `pending_file_requests` module shipped
in commit `e729833`.)

Everything else gets fixed.
