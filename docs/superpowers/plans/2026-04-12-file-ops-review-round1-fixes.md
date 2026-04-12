# Device File Operations — Round 1 Review Fixes

Follow-up plan from the first review-loop round on `feature/file-operations`. Three
Codex reviewers surfaced 31 findings; 22 of them are confirmed fixes and are
tracked in the sibling `.tasks.json`. See that file for the authoritative
progress state — this document is a short reference only.

## Review summary

- **Critical**: 4 (glob escape, symlink bypass, strict-mode approval bypass, pending-request key collision)
- **Important**: 23 (dangerous-path approval, read limits, text/image edge bugs, u64 overflow, Windows compile, missing tests, …)
- **Minor**: 4 (base64 decoder, field rename, missing None-op test, pagination timing)

## Skipped (with reasons)

| Finding | Why skipped |
|---|---|
| FileResponse construction duplicated across layers | Minor refactor; not blocking |
| `FileResponse::move_result` vs spec's `move` | `move` is a Rust keyword; renaming breaks generated code |
| Pagination test 2ms sleep | Not flaking yet; low risk |
| STOP_REASON_ERROR unemitted | Top-level FileError is the equivalent correct path |
| Cross-filesystem move (EXDEV) untested | Needs multi-FS test infra; deferred to follow-up PR |

## Task groups

1. **Proto & HTTP surface** (T0, T1) — gate S3 flow, swap base64 wrapper for
   `application/x-protobuf`.
2. **Policy & dispatch** (T2–T5) — canonicalize paths, glob re-check,
   approval plumbing, `max_read_bytes` enforcement.
3. **Handlers** (T6–T10) — text pagination, raw-byte offsets, u64 overflow,
   image bomb guard + passthrough, `no_follow_symlink`.
4. **Hub correctness** (T11, T12) — scoped pending-request key, admission
   control.
5. **Portability & UX** (T13, T14, T15) — encoding gate, Windows cfg, file
   capability.
6. **Tests** (T16–T21) — hub axum tests, daemon coverage batches, tiny holes,
   `s3.rs` unit tests.

## Convergence target

Round 1 had 4 Critical + 20+ Important. Round 2 target: 0 Critical, ≤3
Important (docs / minor additions only).
