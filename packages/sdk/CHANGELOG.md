# @ahandai/sdk Changelog

## 0.3.0 — 2026-06-11

Released alongside `@ahandai/proto@0.3.0`.

### Added

- **`listAppTools(deviceId, opts?)` / `invokeAppTool(deviceId, name, args?, opts?)`** —
  discover and invoke application-defined tools registered by host apps
  embedding `ahandd`. Exported public types: **`AppToolCatalog`**,
  **`AppToolInfo`**, **`ListAppToolsOptions`**, **`InvokeAppToolOptions`**.

  - `listAppTools` returns an `AppToolCatalog` (numeric `revision`,
    `stale` flag, `updatedAtMs` timestamp, `tools` array of `AppToolInfo`).
  - `invokeAppTool` resolves with the daemon's `result` value on success.
    When the daemon returns a successful response but omits `result`, `null`
    is returned for a stable contract (callers can use `result ?? default`).
  - Daemon-level failures throw `CloudClientError("app_tool_error")` with
    the daemon code in `jobErrorCode`. Daemon error code set:
    `TOOL_NOT_FOUND | INVALID_ARGS | INVALID_CONTEXT | SESSION_INACTIVE |
    APPROVAL_DENIED | APPROVAL_TIMEOUT | EXECUTION_TIMEOUT | HANDLER_PANIC |
    HANDLER_ERROR | CONCURRENCY_LIMIT`.
  - Hub-level error codes reuse existing taxonomy:
    `409 DEVICE_OFFLINE → device_offline` (same as `files()`),
    `504 → timeout` (same as `files()` / `browser()`).
  - Strict root-shape guard — null / non-object / array roots throw
    `CloudClientError("server_error", ...)` rather than propagating raw
    `TypeError`s. Same coercion-masks-regression philosophy as `files()`.
  - `listAppTools` additionally requires an array `tools` field; a numeric
    `revision` alone is insufficient.

## 0.2.2 — 2026-05-13

### Added

- Added `CloudClient.readPdf(params)` and PDF-aware `readFile()` routing.
  PDF reads now return metadata plus mode-specific payloads: raw PDF bytes,
  rendered page images, page-separated text, or the default first-5-page
  preview.

## 0.2.0 — 2026-04-30

Released alongside `@ahandai/proto@0.2.0`. Both packages are now published
via CI (`.github/workflows/release-sdk.yml`) on a `release-v<semver>` tag
push, using npm trusted-publisher OIDC (no long-lived token).

### Breaking

- **Renamed WS-side `BrowserResult` → `DeviceBrowserResult`** (`connection.ts`).
  The name `BrowserResult` is now reused for the new HTTP-side return type of
  `CloudClient.browser()` (cloud-client.ts), with a different shape:

  | Before (WS / `connection.ts`)        | After (HTTP / `cloud-client.ts`)              |
  |---------------------------------------|------------------------------------------------|
  | `binaryData?: Buffer`                 | `binary?: { data: Uint8Array; mime: string }` |
  | `binaryMime?: string`                 | (subsumed into `binary.mime`)                  |
  | (no duration)                         | `durationMs: number`                           |

  External consumers importing `BrowserResult` from `@ahandai/sdk` get the
  new HTTP-side shape and must update field accesses accordingly. Within the
  monorepo, only `team9-agent-pi`'s `claw-hive` package consumes the SDK
  cloud-side surface; `team9-agent-pi` is updated in lockstep.

### Added

- **`CloudClient.browser(params)`** — new method posting to
  `POST /api/control/browser`. Decodes `binary_data` (base64 string) into
  `Uint8Array`. Supports `AbortSignal`. Lazy `getAuthToken()` semantics
  matching `spawn()`.
- **`BrowserParams` / `BrowserResult`** — new public types for the above.
- **`CloudClient.files(params)`** — new method posting to
  `POST /api/control/files`. Single request-response that dispatches one
  of 14 file operations (`stat`, `list`, `glob`, `read_text`,
  `read_binary`, `read_image`, `write`, `edit`, `delete`, `chmod`,
  `mkdir`, `copy`, `move`, `create_symlink`) to a connected device.
  Daemon-level errors (e.g. `not_found`, `policy_denied`) come back
  inside the resolved `FileResult` envelope (`success: false` plus an
  `error` field); hub-level errors (auth, offline, timeout, rate limit)
  are thrown as `CloudClientError`. Same lazy `getAuthToken()` semantics
  as `browser()`. Supports `AbortSignal` and `correlationId` for
  forward-compat with hub-side dedupe.
- **`FileOperation` / `FileParams` / `FileResult` / `FileErrorPayload`** —
  new public types for the above. `params` and `result` are typed as
  `Record<string, unknown>` / `unknown`: per-op shapes are documented in
  the JSDoc and consumers cast to their own per-op types as needed.
- **`"timeout"` `CloudClientErrorCode`** — HTTP 504 from the hub now maps
  to this code (was previously folded into `server_error`). Existing
  `spawn()` consumers are unaffected because `spawn()` surfaces hub
  timeouts via SSE error events, not HTTP status.
- **`"device_offline"` `CloudClientErrorCode`** — HTTP 409 with body
  `{error:{code:"DEVICE_OFFLINE"}}` (returned by the files endpoint when
  a known device is not currently connected) maps to this code. Other
  409s fall through to `bad_request`. Existing `spawn()` / `browser()`
  consumers are unaffected because neither endpoint returns 409.
- **`"policy_denied"` `CloudClientErrorCode`** — HTTP 403 with body
  `{error:{code:"POLICY_DENIED"}}` maps to this code. The hub elevates
  daemon-side `policy_denied` file errors to a hub-level 403 (other
  daemon errors like `not_found` / `io` stay inside the
  `success: false` body) so consumers can branch on
  `err.code === "policy_denied"` without inspecting the response
  envelope. Plain 403 + `FORBIDDEN` / `NOT_DEVICE_OWNER` still maps
  to `forbidden`.
- **Strict response shape validation** — `browser()` and `files()`
  reject malformed hub responses (null / non-object root, array root,
  missing or non-boolean `success`) with
  `CloudClientError("server_error", ...)`. Same for response-body
  parse failures: `SyntaxError → server_error`, `AbortError → abort`,
  other → `network`.

### Notes

- `correlation_id` on `browser()` requests is accepted by the hub's wire
  schema but is **not currently deduplicated** at the hub layer. Workers
  may set the field today as forward-compat (it is reserved on the wire
  for a future minor release that lands dedupe), but must not assume two
  calls with the same id are guaranteed to be deduped today. Tracked as
  follow-up #3 in the cross-repo browser-tool spec
  (`team9-agent-pi/docs/superpowers/specs/2026-04-26-claw-hive-ahand-browser-tool-design.md`).
