//! `CloudClient` wraps the ahand hub control-plane REST + SSE surface
//! (`/api/control/*`). It is consumed by the team9 im-worker (via
//! `AHandHostComponent`) and potentially by the Tauri dashboard — both
//! environments have `globalThis.fetch` and `ReadableStream`, so the
//! SDK uses `fetch` directly (no `EventSource`, which doesn't support
//! custom Authorization headers in browsers).
//!
//! The hub's wire contract (Task 1.4):
//!   * POST `/api/control/jobs` — body is camelCase
//!     `{deviceId, tool, args, cwd, env, timeoutMs, interactive,
//!      correlationId}`; success returns `{jobId: string}` (201/202
//!     on create, 200 on idempotent replay).
//!   * GET  `/api/control/jobs/{id}/stream` — SSE with camelCase
//!     JSON payloads per event. Event types: `stdout` `{chunk}`,
//!     `stderr` `{chunk}`, `progress` `{percent, message?}`,
//!     `finished` `{exitCode, durationMs}`, `error` `{code, message}`.
//!     Keepalives are `:keepalive\n\n` SSE comments. The stream ends
//!     after `finished` or `error`.
//!   * POST `/api/control/jobs/{id}/cancel` — returns 202 with no body.
//!   * Error envelope on 4xx/5xx: `{error: {code, message}}`.
//!
//! Auth is a control-plane JWT provided via `getAuthToken()`. It's
//! invoked lazily on every POST so callers can implement
//! refresh-on-401 externally. We do NOT cache tokens internally.

/** Options accepted by the `CloudClient` constructor. */
export interface CloudClientOptions {
  /** Base URL of the hub, e.g. `https://hub.example.com`. No trailing slash. */
  hubUrl: string;
  /**
   * Lazy auth-token provider. Invoked on every POST (including the
   * SSE subscription). Throw to fail the whole spawn; return a fresh
   * token to refresh. Errors propagate to `spawn()` / `cancel()`.
   */
  getAuthToken: () => Promise<string>;
  /**
   * Optional fetch override, primarily for tests. Defaults to
   * `globalThis.fetch`. Must implement the standard `fetch` contract
   * (AbortSignal honoured, `Response.body` is a `ReadableStream`).
   */
  fetch?: typeof fetch;
}

/** Parameters for a single `spawn()` invocation. */
export interface SpawnParams {
  deviceId: string;
  /** Executable name (matches hub `tool` field). */
  tool: string;
  /** Arguments; defaults to []. */
  args?: string[];
  cwd?: string;
  /** Environment variables (maps to hub `env` field). */
  env?: Record<string, string>;
  timeoutMs?: number;
  /**
   * Idempotency key. Passing the same key twice while a job is still
   * live returns the existing `jobId` without re-dispatching.
   */
  correlationId?: string;
  /** Whether the job should run attached to a PTY. Defaults to false. */
  interactive?: boolean;

  /** Callback for stdout chunks. Thrown errors are swallowed. */
  onStdout?: (chunk: string) => void;
  /** Callback for stderr chunks. Thrown errors are swallowed. */
  onStderr?: (chunk: string) => void;
  /** Callback for progress events. Thrown errors are swallowed. */
  onProgress?: (p: { percent: number; message?: string }) => void;
  /**
   * Callback fired after the hub accepts the job and returns its id,
   * before the SDK opens the SSE stream. Thrown errors are swallowed.
   */
  onJobStarted?: (job: { jobId: string }) => void;
  /**
   * AbortSignal — aborting triggers best-effort
   * `POST /cancel` + closes SSE + rejects with an `abort` error.
   */
  signal?: AbortSignal;
}

/** Resolved result of a successful `spawn()`. */
export interface SpawnResult {
  exitCode: number;
  durationMs: number;
}

/**
 * File operations the daemon supports. Each name maps 1:1 to a
 * variant of the protobuf `FileRequest.operation` oneof; the hub
 * translates the JSON request body into the right proto variant. Use
 * the per-op shape below to fill `FileParams.params`.
 */
export type FileOperation =
  | "stat"
  | "list"
  | "glob"
  | "read_text"
  | "read_binary"
  | "read_image"
  | "read_pdf"
  | "write"
  | "edit"
  | "delete"
  | "chmod"
  | "mkdir"
  | "copy"
  | "move"
  | "create_symlink";

/**
 * Parameters for a single `files()` invocation. Maps to the hub's
 * `POST /api/control/files` request body (snake_case wire format).
 *
 * `params` holds the operation-specific fields and is forwarded to the
 * hub verbatim (the hub validates against the proto schema). The SDK
 * does NOT do client-side path validation — the daemon's policy engine
 * is the source of truth, so consumers should expect a `success: false`
 * result with `error.code === "policy_denied"` for refused paths.
 *
 * Examples of well-formed `params` for the most common ops (this is a
 * subset — see the hub's `http::control_files_dto` module for the full
 * field-by-field schema):
 *
 *   * `stat`:         `{ path: "/tmp/x", no_follow_symlink?: boolean }`
 *   * `list`:         `{ path: "/tmp", max_results?, offset?, include_hidden? }`
 *   * `read_text`:    `{ path, start?: { start_line: 1 }, max_lines?, line_numbers? }`
 *   * `read_pdf`:     `{ path, mode?: "auto" | "metadata" | "raw" | "imgs" | "text", page_range? }`
 *   * `write` (full): `{ path, create_parents?, full_write: { content: "..." } }`
 *   * `delete`:       `{ path, recursive?, mode?: "trash" | "permanent" }`
 */
export interface FileParams {
  deviceId: string;
  operation: FileOperation;
  /**
   * Operation-specific parameters. Defaults to `{}` if omitted (the
   * stat/glob/read_* ops require at least a `path`/`pattern` field, so
   * an empty object will yield a 400 INVALID_PARAMS from the hub).
   */
  params?: Record<string, unknown>;
  /** Per-request timeout. Defaults to 30 000 ms when not provided. */
  timeoutMs?: number;
  /**
   * Idempotency / tracing key. Forwarded as `correlation_id` on the
   * wire; sent only when provided. Hub-side dedupe is a follow-up;
   * today the field is opaque metadata.
   */
  correlationId?: string;
  /**
   * AbortSignal — aborting cancels the in-flight fetch and rejects with
   * a `CloudClientError` whose `code === "abort"`.
   */
  signal?: AbortSignal;
}

/** Daemon-side error envelope returned inside a `FileResult`. */
export interface FileErrorPayload {
  /**
   * Lower-snake-case error code. Stable wire values include:
   * `not_found`, `permission_denied`, `already_exists`, `not_a_directory`,
   * `is_a_directory`, `not_empty`, `too_large`, `invalid_path`, `io`,
   * `encoding`, `multiple_matches`, `policy_denied`, `unspecified`.
   */
  code: string;
  message: string;
  /** Path the daemon was operating on when the error fired (may be ""). */
  path: string;
}

/**
 * Resolved result of a successful `files()` HTTP round-trip. Hub-level
 * failures (auth, offline, timeout) are thrown as `CloudClientError`;
 * daemon-level failures (file not found, policy refusal, ...) come back
 * as `success: false` with an `error` field populated. This is
 * symmetric with `BrowserResult.error` and matches the dashboard's
 * protobuf-envelope semantics.
 *
 * `result` shape mirrors the proto result for the requested op (see
 * `proto/ahand/v1/file_ops.proto`). All field names are snake_case to
 * match the wire — the SDK does NOT do per-op typed unwrapping. Cast
 * to your own per-op types as needed.
 */
export interface FileResult {
  /** Hub-minted UUID echoed back so callers can correlate logs. */
  requestId: string;
  /** Operation tag the caller sent ("stat", "list", ...). */
  operation: string;
  /**
   * `true` when the daemon completed the op successfully (in which
   * case `result` is set). `false` when the daemon refused or hit a
   * filesystem error (in which case `error` is set).
   */
  success: boolean;
  /** Operation-specific result body, present when `success === true`. */
  result?: unknown;
  /** Daemon error envelope, present when `success === false`. */
  error?: FileErrorPayload;
  /** Hub-measured wall-clock latency for the round trip. */
  durationMs: number;
}

export interface FileUploadUrlParams {
  deviceId: string;
  signal?: AbortSignal;
}

export interface FileUploadUrlResult {
  objectKey: string;
  uploadUrl: string;
  expiresAtMs: number;
}

export type ReadFileMode = "auto" | "text" | "image" | "binary";

export type ReadPdfMode = "auto" | "metadata" | "raw" | "imgs" | "text";

export type ReadFileImageFormat = "original" | "jpeg" | "png" | "webp";

export interface ReadFileParams {
  deviceId: string;
  path: string;
  mode?: ReadFileMode;
  /** Zero-based byte cursor. Cannot be used with startLine. */
  startIndex?: number;
  /** Zero-based line cursor for text reads. Cannot be used with startIndex. */
  startLine?: number;
  /** Text max_bytes / binary max_bytes, depending on mode. */
  maxSize?: number;
  /** Text max_lines. */
  maxLine?: number;
  /** Text max_line_width; 0 disables per-line truncation daemon-side. */
  maxLineWidth?: number;
  encoding?: string;
  /** Binary byte_offset. Defaults to 0. */
  byteOffset?: number;
  /** Binary byte_length. 0 means read to EOF subject to maxSize. */
  byteLength?: number;
  /** Image longest-edge convenience. Maps to max_width and max_height. */
  maxEdge?: number;
  maxWidth?: number;
  maxHeight?: number;
  maxBytes?: number;
  quality?: number;
  imageFormat?: ReadFileImageFormat;
  noFollowSymlink?: boolean;
  timeoutMs?: number;
  correlationId?: string;
  signal?: AbortSignal;
}

export interface ReadFilePosition {
  line: number;
  byteInFile: number;
  byteInLine: number;
}

export interface ReadFileTextLine {
  content: string;
  lineNumber: number;
  truncated: boolean;
  remainingBytes: number;
}

export interface ReadFileTextResult {
  kind: "text";
  path: string;
  requestId: string;
  operation: "read_text";
  content: string;
  lines: ReadFileTextLine[];
  stopReason:
    | "unspecified"
    | "max_lines"
    | "max_bytes"
    | "target_end"
    | "file_end"
    | "error";
  start: ReadFilePosition | null;
  end: ReadFilePosition | null;
  remainingBytes: number;
  totalBytes: number;
  totalLines: number;
  detectedEncoding: string;
  truncated: boolean;
  cursor?: string;
  durationMs: number;
}

export interface ReadFileBinaryResult {
  kind: "binary";
  path: string;
  requestId: string;
  operation: "read_binary";
  data: Uint8Array;
  mime: string;
  byteOffset: number;
  bytesRead: number;
  totalBytes: number;
  remainingBytes: number;
  durationMs: number;
}

export interface ReadFileImageResult {
  kind: "image";
  path: string;
  requestId: string;
  operation: "read_image";
  data: Uint8Array;
  mime: string;
  format: ReadFileImageFormat;
  width: number;
  height: number;
  originalBytes: number;
  outputBytes: number;
  durationMs: number;
}

export interface ReadPdfMetadata {
  path: string;
  totalBytes: number;
  totalPages: number;
}

export interface ReadPdfPageRange {
  startPage: number;
  endPage: number;
}

export interface ReadPdfPageImage {
  pageNumber: number;
  data: Uint8Array;
  mime: string;
  format: ReadFileImageFormat;
  width: number;
  height: number;
  outputBytes: number;
}

export interface ReadPdfPageText {
  pageNumber: number;
  content: string;
}

export interface ReadPdfParams {
  deviceId: string;
  path: string;
  mode?: ReadPdfMode;
  /** 1-based page range, e.g. "1-5" or { startPage: 1, endPage: 5 }. */
  pages?: string | ReadPdfPageRange;
  noFollowSymlink?: boolean;
  timeoutMs?: number;
  correlationId?: string;
  signal?: AbortSignal;
}

export interface ReadFilePdfResult {
  kind: "pdf";
  path: string;
  requestId: string;
  operation: "read_pdf";
  mode: ReadPdfMode;
  metadata: ReadPdfMetadata;
  pageRange: ReadPdfPageRange | null;
  raw?: { data: Uint8Array; mime: "application/pdf"; totalBytes: number };
  images: ReadPdfPageImage[];
  textPages: ReadPdfPageText[];
  durationMs: number;
}

export type ReadFileResult =
  | ReadFileTextResult
  | ReadFileBinaryResult
  | ReadFileImageResult
  | ReadFilePdfResult;

/**
 * Parameters for a single `browser()` invocation. Maps to the hub's
 * `POST /api/control/browser` request body (snake_case wire format).
 */
export interface BrowserParams {
  deviceId: string;
  /** Browser session identifier (derived by HostComponent). */
  sessionId: string;
  /** Action verb, e.g. `"open"`, `"click"`, `"snapshot"`, `"screenshot"`. */
  action: string;
  /**
   * Action-specific parameters. Sent verbatim as the `params` JSON
   * object on the wire — defaults to `{}` if omitted (NOT dropped).
   */
  params?: Record<string, unknown>;
  /** Per-request timeout. Defaults to 30 000 ms when not provided. */
  timeoutMs?: number;
  /**
   * Idempotency / tracing key. Forwarded as `correlation_id` on the
   * wire; sent only when provided.
   */
  correlationId?: string;
  /**
   * AbortSignal — aborting cancels the in-flight fetch and rejects with
   * a `CloudClientError` whose `code === "abort"`.
   */
  signal?: AbortSignal;
}

/**
 * Resolved result of a successful `browser()` call. Decoded from the
 * hub's snake_case JSON response: `binary_data` (base64) → `Uint8Array`
 * in `binary.data`; `binary_mime` → `binary.mime`. When the hub omits
 * `binary_data` (or sends an empty string), `binary` is `undefined`.
 */
export interface BrowserResult {
  success: boolean;
  /** Parsed `result_json` from the device, or `undefined` when absent. */
  data?: unknown;
  /** Hub-supplied error string (only set when `success === false`). */
  error?: string;
  binary?: { data: Uint8Array; mime: string };
  durationMs: number;
}

// ---------------------------------------------------------------------------
// App-tool types
// ---------------------------------------------------------------------------

/**
 * Metadata for a single app-defined tool registered by a host app embedding
 * `ahandd`. The `inputSchemaJson` field is a JSON-encoded JSON Schema
 * (string, not an object) matching the shape the tool expects as `args`.
 */
export interface AppToolInfo {
  name: string;
  description: string;
  inputSchemaJson: string;
  requiresApproval: boolean;
}

/**
 * The full catalog of app-defined tools registered by a device. `revision`
 * is a monotonically-increasing counter bumped by the daemon each time the
 * catalog changes. `stale` is `true` when the hub's cached copy has not
 * been refreshed since the last heartbeat timeout.
 */
export interface AppToolCatalog {
  revision: number;
  stale: boolean;
  updatedAtMs: number;
  tools: AppToolInfo[];
}

/** Options for `listAppTools()`. */
export interface ListAppToolsOptions {
  signal?: AbortSignal;
}

/** Options for `invokeAppTool()`. */
export interface InvokeAppToolOptions {
  /**
   * Per-request timeout in milliseconds. Defaults to `60_000` (60 s)
   * when omitted. The hub clamps the value to `[1_000, 300_000]`
   * (1 s – 5 min).
   */
  timeoutMs?: number;
  signal?: AbortSignal;
}

/** Discriminated error codes surfaced by `CloudClient`. */
export type CloudClientErrorCode =
  | "unauthorized"
  | "forbidden"
  | "not_found"
  | "rate_limited"
  | "bad_request"
  | "server_error"
  | "stream_ended"
  | "job_error"
  | "abort"
  | "network"
  | "timeout"
  | "device_offline"
  | "policy_denied"
  | "s3_disabled"
  | "app_tool_error";

/**
 * Typed error raised by `CloudClient`. Use `.code` to discriminate,
 * `.httpStatus` for HTTP-sourced errors, and `.jobErrorCode` /
 * `.jobErrorMessage` for SSE `error` events forwarded from the hub.
 */
export class CloudClientError extends Error {
  readonly code: CloudClientErrorCode;
  readonly httpStatus?: number;
  /**
   * Daemon error code forwarded by the hub. Carried for SSE `job_error`
   * events (`spawn()`), file-op daemon errors (`files()`), and
   * app-tool daemon errors (`invokeAppTool()`).
   */
  readonly jobErrorCode?: string;
  /**
   * Daemon error message forwarded by the hub. Carried for SSE
   * `job_error` events, file-op daemon errors, and app-tool daemon
   * errors. May be `undefined` when the daemon omits a message.
   */
  readonly jobErrorMessage?: string;
  /** Original error, if this wraps one (e.g. `network`). */
  readonly cause?: unknown;

  constructor(
    code: CloudClientErrorCode,
    message: string,
    extras?: {
      httpStatus?: number;
      jobErrorCode?: string;
      jobErrorMessage?: string;
      cause?: unknown;
    },
  ) {
    super(message);
    this.name = "CloudClientError";
    this.code = code;
    this.httpStatus = extras?.httpStatus;
    this.jobErrorCode = extras?.jobErrorCode;
    this.jobErrorMessage = extras?.jobErrorMessage;
    this.cause = extras?.cause;
  }
}

/** Hub JSON error envelope. */
interface HubErrorEnvelope {
  error?: { code?: string; message?: string };
}

interface ParsedSseEvent {
  event: string;
  data: string;
}

/**
 * Classify a non-OK fetch response into a `CloudClientError`. Tries to
 * parse the hub's `{error: {code, message}}` envelope for a useful
 * message; falls back to the HTTP status text on parse failure.
 */
async function toTypedHttpError(
  res: Response,
  signal?: AbortSignal,
): Promise<CloudClientError> {
  let code: CloudClientErrorCode;
  switch (res.status) {
    case 400:
      code = "bad_request";
      break;
    case 401:
      code = "unauthorized";
      break;
    case 403:
      code = "forbidden";
      break;
    case 404:
      code = "not_found";
      break;
    case 409:
      // Files-endpoint hub contract: 409 with body {error:{code:"DEVICE_OFFLINE"}}
      // means the device is registered but not currently connected over WS.
      // Other 409s (none today) fall through to bad_request below after the
      // body is inspected.
      code = "bad_request";
      break;
    case 429:
      code = "rate_limited";
      break;
    case 504:
      code = "timeout";
      break;
    default:
      code = res.status >= 500 ? "server_error" : "bad_request";
  }
  let message = `${res.status} ${res.statusText || ""}`.trim();
  try {
    const body = (await res.json()) as HubErrorEnvelope;
    if (body?.error?.message) {
      message = body.error.message;
    }
    // Inspect the hub's discriminator code for two surface-elevated
    // cases so a SDK consumer can branch on `err.code` without
    // string-matching the message:
    //   * `device_offline` — files endpoint, 409 + DEVICE_OFFLINE.
    //   * `policy_denied`  — files endpoint, 403 + POLICY_DENIED
    //     (daemon refused the operation by policy; surfaced at the
    //     hub layer so callers don't have to inspect the result body).
    if (res.status === 409 && body?.error?.code === "DEVICE_OFFLINE") {
      code = "device_offline";
    } else if (res.status === 403 && body?.error?.code === "POLICY_DENIED") {
      code = "policy_denied";
    } else if (body?.error?.code === "S3_DISABLED") {
      code = "s3_disabled";
    }
  } catch (err) {
    if (signal?.aborted) {
      return new CloudClientError("abort", "Aborted", { cause: err });
    }
    // Body wasn't JSON — keep the status-based message.
  }
  return new CloudClientError(code, message, { httpStatus: res.status });
}

/**
 * Parse a raw SSE event block (text between two `\n\n` separators).
 * Per the SSE spec:
 *   - Lines starting with `:` are comments (keepalives) → caller
 *     skips these via `isSseComment()` before reaching the parser.
 *   - `event: <name>` sets the event name (default `"message"`).
 *   - `data: <payload>` lines are concatenated with `\n`.
 *   - `\r\n` is treated as `\n`.
 */
function parseSseEvent(raw: string): ParsedSseEvent {
  let event = "message";
  const dataLines: string[] = [];
  // Normalize CRLF → LF to tolerate servers / proxies that use either.
  for (const rawLine of raw.replace(/\r\n/g, "\n").split("\n")) {
    if (rawLine === "" || rawLine.startsWith(":")) continue;
    const colon = rawLine.indexOf(":");
    // A line without a colon is a field name with an empty value. We
    // only care about `event` / `data`, so lines like "retry" are no-ops.
    const field = colon === -1 ? rawLine : rawLine.slice(0, colon);
    // Per spec, one optional space after the colon is stripped.
    let value = colon === -1 ? "" : rawLine.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    if (field === "event") {
      event = value;
    } else if (field === "data") {
      dataLines.push(value);
    }
    // All other fields (`id`, `retry`, unknown) are ignored.
  }
  return { event, data: dataLines.join("\n") };
}

/** Is this a pure SSE comment block (e.g. axum's `:keepalive`)? */
function isSseComment(raw: string): boolean {
  // An SSE comment block is one or more lines all starting with `:`.
  // We only need to detect the common keepalive case: a single line
  // beginning with `:` and no `data:` / `event:` lines.
  // A rawEvent consisting entirely of empty lines is NOT a comment —
  // it must have at least one non-empty line starting with `:`.
  const normalized = raw.replace(/\r\n/g, "\n");
  let sawNonEmpty = false;
  for (const line of normalized.split("\n")) {
    if (line === "") continue;
    sawNonEmpty = true;
    if (!line.startsWith(":")) return false;
  }
  return sawNonEmpty;
}

const IMAGE_PATH_RE = /\.(?:png|jpe?g|webp|gif|bmp|tiff?|avif|heic|heif)$/i;
const PDF_PATH_RE = /\.pdf(?:[?#].*)?$/i;

interface ReadTextWireResult {
  lines: TextLineWire[];
  stop_reason:
    | "unspecified"
    | "max_lines"
    | "max_bytes"
    | "target_end"
    | "file_end"
    | "error";
  start_pos: PositionInfoWire | null;
  end_pos: PositionInfoWire | null;
  remaining_bytes: number;
  total_file_bytes: number;
  total_lines: number;
  detected_encoding: string;
}

interface TextLineWire {
  content: string;
  line_number: number;
  truncated: boolean;
  remaining_bytes: number;
}

interface PositionInfoWire {
  line: number;
  byte_in_file: number;
  byte_in_line: number;
}

interface ReadBinaryWireResult {
  content_b64?: string;
  byte_offset: number;
  bytes_read: number;
  total_file_bytes: number;
  remaining_bytes: number;
  download_url?: string | null;
  download_url_expires_ms?: number | null;
}

interface ReadImageWireResult {
  content_b64?: string;
  format: ReadFileImageFormat;
  width: number;
  height: number;
  original_bytes: number;
  output_bytes: number;
  download_url?: string | null;
  download_url_expires_ms?: number | null;
}

interface ReadPdfMetadataWire {
  path: string;
  total_file_bytes: number;
  total_pages: number;
}

interface ReadPdfPageRangeWire {
  start_page: number;
  end_page: number;
}

interface ReadPdfPageImageWire {
  page_number: number;
  content_b64: string;
  format: ReadFileImageFormat;
  width: number;
  height: number;
  output_bytes: number;
}

interface ReadPdfPageTextWire {
  page_number: number;
  content: string;
}

interface ReadPdfWireResult {
  mode: ReadPdfMode;
  metadata: ReadPdfMetadataWire;
  page_range?: ReadPdfPageRangeWire | null;
  raw_content_b64?: string | null;
  images: ReadPdfPageImageWire[];
  text_pages: ReadPdfPageTextWire[];
}

const READ_TEXT_STOP_REASONS = [
  "unspecified",
  "max_lines",
  "max_bytes",
  "target_end",
  "file_end",
  "error",
] as const;

const READ_IMAGE_FORMATS = ["original", "jpeg", "png", "webp"] as const;
const READ_PDF_MODES = ["auto", "metadata", "raw", "imgs", "text"] as const;

function isRecord(x: unknown): x is Record<string, unknown> {
  return typeof x === "object" && x !== null && !Array.isArray(x);
}

function requireReadTextWireResult(x: unknown): ReadTextWireResult {
  if (!isRecord(x)) {
    throw malformedFilePayload("read_text", "result is not an object");
  }
  if (!Array.isArray(x.lines) || !x.lines.every(isTextLineWire)) {
    throw malformedFilePayload("read_text", "lines has invalid shape");
  }
  if (
    typeof x.stop_reason !== "string" ||
    !(READ_TEXT_STOP_REASONS as readonly string[]).includes(x.stop_reason)
  ) {
    throw malformedFilePayload("read_text", "stop_reason has invalid shape");
  }
  if (x.start_pos !== null && x.start_pos !== undefined && !isPositionInfoWire(x.start_pos)) {
    throw malformedFilePayload("read_text", "start_pos has invalid shape");
  }
  if (x.end_pos !== null && x.end_pos !== undefined && !isPositionInfoWire(x.end_pos)) {
    throw malformedFilePayload("read_text", "end_pos has invalid shape");
  }
  if (typeof x.remaining_bytes !== "number") {
    throw malformedFilePayload("read_text", "remaining_bytes has invalid shape");
  }
  if (typeof x.total_file_bytes !== "number") {
    throw malformedFilePayload("read_text", "total_file_bytes has invalid shape");
  }
  if (typeof x.total_lines !== "number") {
    throw malformedFilePayload("read_text", "total_lines has invalid shape");
  }
  if (typeof x.detected_encoding !== "string") {
    throw malformedFilePayload("read_text", "detected_encoding has invalid shape");
  }
  return x as unknown as ReadTextWireResult;
}

function isTextLineWire(x: unknown): x is TextLineWire {
  if (!isRecord(x)) return false;
  return (
    typeof x.content === "string" &&
    typeof x.line_number === "number" &&
    typeof x.truncated === "boolean" &&
    typeof x.remaining_bytes === "number"
  );
}

function isPositionInfoWire(x: unknown): x is PositionInfoWire {
  if (!isRecord(x)) return false;
  return (
    typeof x.line === "number" &&
    typeof x.byte_in_file === "number" &&
    typeof x.byte_in_line === "number"
  );
}

function requireReadBinaryWireResult(x: unknown): ReadBinaryWireResult {
  if (!isRecord(x)) {
    throw malformedFilePayload("read_binary", "result is not an object");
  }
  if (x.content_b64 !== undefined && typeof x.content_b64 !== "string") {
    throw malformedFilePayload("read_binary", "content_b64 has invalid shape");
  }
  if (typeof x.byte_offset !== "number") {
    throw malformedFilePayload("read_binary", "byte_offset has invalid shape");
  }
  if (typeof x.bytes_read !== "number") {
    throw malformedFilePayload("read_binary", "bytes_read has invalid shape");
  }
  if (typeof x.total_file_bytes !== "number") {
    throw malformedFilePayload("read_binary", "total_file_bytes has invalid shape");
  }
  if (typeof x.remaining_bytes !== "number") {
    throw malformedFilePayload("read_binary", "remaining_bytes has invalid shape");
  }
  if (x.download_url !== null && x.download_url !== undefined && typeof x.download_url !== "string") {
    throw malformedFilePayload("read_binary", "download_url has invalid shape");
  }
  return x as unknown as ReadBinaryWireResult;
}

function requireReadImageWireResult(x: unknown): ReadImageWireResult {
  if (!isRecord(x)) {
    throw malformedFilePayload("read_image", "result is not an object");
  }
  if (x.content_b64 !== undefined && typeof x.content_b64 !== "string") {
    throw malformedFilePayload("read_image", "content_b64 has invalid shape");
  }
  if (
    typeof x.format !== "string" ||
    !(READ_IMAGE_FORMATS as readonly string[]).includes(x.format)
  ) {
    throw malformedFilePayload("read_image", "format has invalid shape");
  }
  if (typeof x.width !== "number" || typeof x.height !== "number") {
    throw malformedFilePayload("read_image", "dimensions have invalid shape");
  }
  if (typeof x.original_bytes !== "number" || typeof x.output_bytes !== "number") {
    throw malformedFilePayload("read_image", "byte counts have invalid shape");
  }
  if (x.download_url !== null && x.download_url !== undefined && typeof x.download_url !== "string") {
    throw malformedFilePayload("read_image", "download_url has invalid shape");
  }
  return x as unknown as ReadImageWireResult;
}

function requireReadPdfWireResult(x: unknown): ReadPdfWireResult {
  if (!isRecord(x)) {
    throw malformedFilePayload("read_pdf", "result is not an object");
  }
  if (
    typeof x.mode !== "string" ||
    !(READ_PDF_MODES as readonly string[]).includes(x.mode)
  ) {
    throw malformedFilePayload("read_pdf", "mode has invalid shape");
  }
  if (!isReadPdfMetadataWire(x.metadata)) {
    throw malformedFilePayload("read_pdf", "metadata has invalid shape");
  }
  if (
    x.page_range !== null &&
    x.page_range !== undefined &&
    !isReadPdfPageRangeWire(x.page_range)
  ) {
    throw malformedFilePayload("read_pdf", "page_range has invalid shape");
  }
  if (
    x.raw_content_b64 !== null &&
    x.raw_content_b64 !== undefined &&
    typeof x.raw_content_b64 !== "string"
  ) {
    throw malformedFilePayload("read_pdf", "raw_content_b64 has invalid shape");
  }
  if (!Array.isArray(x.images) || !x.images.every(isReadPdfPageImageWire)) {
    throw malformedFilePayload("read_pdf", "images has invalid shape");
  }
  if (!Array.isArray(x.text_pages) || !x.text_pages.every(isReadPdfPageTextWire)) {
    throw malformedFilePayload("read_pdf", "text_pages has invalid shape");
  }
  return x as unknown as ReadPdfWireResult;
}

function isReadPdfMetadataWire(x: unknown): x is ReadPdfMetadataWire {
  if (!isRecord(x)) return false;
  return (
    typeof x.path === "string" &&
    typeof x.total_file_bytes === "number" &&
    typeof x.total_pages === "number"
  );
}

function isReadPdfPageRangeWire(x: unknown): x is ReadPdfPageRangeWire {
  if (!isRecord(x)) return false;
  return typeof x.start_page === "number" && typeof x.end_page === "number";
}

function isReadPdfPageImageWire(x: unknown): x is ReadPdfPageImageWire {
  if (!isRecord(x)) return false;
  return (
    typeof x.page_number === "number" &&
    typeof x.content_b64 === "string" &&
    typeof x.format === "string" &&
    (READ_IMAGE_FORMATS as readonly string[]).includes(x.format) &&
    typeof x.width === "number" &&
    typeof x.height === "number" &&
    typeof x.output_bytes === "number"
  );
}

function isReadPdfPageTextWire(x: unknown): x is ReadPdfPageTextWire {
  if (!isRecord(x)) return false;
  return typeof x.page_number === "number" && typeof x.content === "string";
}

function malformedFilePayload(operation: string, reason: string): CloudClientError {
  return new CloudClientError(
    "server_error",
    `Hub returned malformed ${operation} payload: ${reason}`,
  );
}

function textPositionFromWire(p: PositionInfoWire | null): ReadFilePosition | null {
  if (!p) return null;
  return {
    line: p.line,
    byteInFile: p.byte_in_file,
    byteInLine: p.byte_in_line,
  };
}

function textCursorFromWire(r: ReadTextWireResult): string | undefined {
  if (r.remaining_bytes <= 0 || !r.end_pos) return undefined;
  if (r.stop_reason === "max_lines") {
    return `startLine=${r.end_pos.line}`;
  }
  return `startIndex=${r.end_pos.byte_in_file}`;
}

function fileErrorCodeToClientCode(code: string): CloudClientErrorCode {
  switch (code) {
    case "not_found":
      return "not_found";
    case "permission_denied":
      return "forbidden";
    case "policy_denied":
      return "policy_denied";
    default:
      return "bad_request";
  }
}

function throwFileResultError(operation: FileOperation, result: FileResult): never {
  const err = result.error;
  const fileCode = err?.code || "unspecified";
  const message = err?.message || `${operation} failed`;
  const pathSuffix = err?.path ? ` (path: ${err.path})` : "";
  throw new CloudClientError(
    fileErrorCodeToClientCode(fileCode),
    `${operation} failed: [${fileCode}] ${message}${pathSuffix}`,
    { jobErrorCode: fileCode, jobErrorMessage: message },
  );
}

function buildReadTextParams(p: ReadFileParams): Record<string, unknown> {
  if (p.startIndex !== undefined && p.startLine !== undefined) {
    throw new CloudClientError(
      "bad_request",
      "readFile startIndex and startLine cannot both be provided",
    );
  }
  const params: Record<string, unknown> = {
    path: p.path,
    line_numbers: true,
    no_follow_symlink: p.noFollowSymlink ?? false,
  };
  if (p.startIndex !== undefined) params.start = { start_byte: p.startIndex };
  if (p.startLine !== undefined) params.start = { start_line: p.startLine + 1 };
  if (p.maxLine !== undefined) params.max_lines = p.maxLine;
  if (p.maxSize !== undefined) params.max_bytes = p.maxSize;
  if (p.maxLineWidth !== undefined) params.max_line_width = p.maxLineWidth;
  if (p.encoding !== undefined && p.encoding.length > 0) params.encoding = p.encoding;
  return params;
}

function buildReadBinaryParams(p: ReadFileParams): Record<string, unknown> {
  return {
    path: p.path,
    byte_offset: p.byteOffset ?? p.startIndex ?? 0,
    byte_length: p.byteLength ?? 0,
    max_bytes: p.maxBytes ?? p.maxSize,
    no_follow_symlink: p.noFollowSymlink ?? false,
  };
}

function buildReadImageParams(p: ReadFileParams): Record<string, unknown> {
  const params: Record<string, unknown> = {
    path: p.path,
    output_format: p.imageFormat ?? "original",
    no_follow_symlink: p.noFollowSymlink ?? false,
  };
  if (p.maxEdge !== undefined) {
    params.max_width = p.maxEdge;
    params.max_height = p.maxEdge;
  }
  if (p.maxWidth !== undefined) params.max_width = p.maxWidth;
  if (p.maxHeight !== undefined) params.max_height = p.maxHeight;
  if (p.maxBytes !== undefined || p.maxSize !== undefined) {
    params.max_bytes = p.maxBytes ?? p.maxSize;
  }
  if (p.quality !== undefined) params.quality = p.quality;
  return params;
}

function normalizePdfPageRange(pages: ReadPdfParams["pages"]): Record<string, number> | undefined {
  if (pages === undefined) return undefined;
  if (typeof pages !== "string") {
    return { start_page: pages.startPage, end_page: pages.endPage };
  }

  const trimmed = pages.trim();
  if (!trimmed) {
    throw new CloudClientError("bad_request", "readPdf pages cannot be empty");
  }
  if (!/^\d+(?:-\d+)?$/.test(trimmed)) {
    throw new CloudClientError("bad_request", `Invalid readPdf pages: ${pages}`);
  }
  const dash = trimmed.indexOf("-");
  if (dash === -1) {
    const page = Number.parseInt(trimmed, 10);
    if (!Number.isInteger(page) || page < 1) {
      throw new CloudClientError("bad_request", `Invalid readPdf pages: ${pages}`);
    }
    return { start_page: page, end_page: page };
  }
  const start = Number.parseInt(trimmed.slice(0, dash), 10);
  const end = Number.parseInt(trimmed.slice(dash + 1), 10);
  if (!Number.isInteger(start) || !Number.isInteger(end) || start < 1 || end < start) {
    throw new CloudClientError("bad_request", `Invalid readPdf pages: ${pages}`);
  }
  return { start_page: start, end_page: end };
}

function buildReadPdfParams(p: ReadPdfParams): Record<string, unknown> {
  const params: Record<string, unknown> = {
    path: p.path,
    mode: p.mode ?? "auto",
    no_follow_symlink: p.noFollowSymlink ?? false,
  };
  const pageRange = normalizePdfPageRange(p.pages);
  if (pageRange) params.page_range = pageRange;
  return params;
}

function mimeFromPath(path: string): string {
  const normalized = path.split(/[?#]/, 1)[0].toLowerCase();
  if (normalized.endsWith(".pdf")) return "application/pdf";
  if (normalized.endsWith(".png")) return "image/png";
  if (normalized.endsWith(".jpg") || normalized.endsWith(".jpeg")) return "image/jpeg";
  if (normalized.endsWith(".webp")) return "image/webp";
  if (normalized.endsWith(".gif")) return "image/gif";
  if (normalized.endsWith(".bmp")) return "image/bmp";
  if (normalized.endsWith(".tif") || normalized.endsWith(".tiff")) return "image/tiff";
  return "application/octet-stream";
}

function sniffImageMime(data: Uint8Array): string | null {
  if (data.length >= 4 && data[0] === 0x89 && data[1] === 0x50 && data[2] === 0x4e && data[3] === 0x47) {
    return "image/png";
  }
  if (data.length >= 3 && data[0] === 0xff && data[1] === 0xd8 && data[2] === 0xff) {
    return "image/jpeg";
  }
  if (
    data.length >= 12 &&
    data[0] === 0x52 &&
    data[1] === 0x49 &&
    data[2] === 0x46 &&
    data[3] === 0x46 &&
    data[8] === 0x57 &&
    data[9] === 0x45 &&
    data[10] === 0x42 &&
    data[11] === 0x50
  ) {
    return "image/webp";
  }
  if (data.length >= 3 && data[0] === 0x47 && data[1] === 0x49 && data[2] === 0x46) {
    return "image/gif";
  }
  return null;
}

function imageMime(format: ReadFileImageFormat, data: Uint8Array, path: string): string {
  switch (format) {
    case "jpeg":
      return "image/jpeg";
    case "png":
      return "image/png";
    case "webp":
      return "image/webp";
    case "original":
    default:
      return sniffImageMime(data) ?? mimeFromPath(path);
  }
}

function normalizeTextReadResult(path: string, result: FileResult): ReadFileTextResult {
  const r = requireReadTextWireResult(result.result);
  const lines = r.lines.map((line) => ({
    content: line.content,
    lineNumber: line.line_number,
    truncated: line.truncated,
    remainingBytes: line.remaining_bytes,
  }));
  const content = lines
    .map((line) => {
      const suffix = line.truncated
        ? ` [line truncated; ${line.remainingBytes} bytes remaining]`
        : "";
      return `${line.lineNumber} | ${line.content}${suffix}`;
    })
    .join("\n");
  const cursor = textCursorFromWire(r);
  return {
    kind: "text",
    path,
    requestId: result.requestId,
    operation: "read_text",
    content,
    lines,
    stopReason: r.stop_reason,
    start: textPositionFromWire(r.start_pos),
    end: textPositionFromWire(r.end_pos),
    remainingBytes: r.remaining_bytes,
    totalBytes: r.total_file_bytes,
    totalLines: r.total_lines,
    detectedEncoding: r.detected_encoding,
    truncated: cursor !== undefined || lines.some((line) => line.truncated),
    ...(cursor ? { cursor } : {}),
    durationMs: result.durationMs,
  };
}

function normalizePdfReadResult(path: string, result: FileResult): ReadFilePdfResult {
  const r = requireReadPdfWireResult(result.result);
  const metadata: ReadPdfMetadata = {
    path: r.metadata.path || path,
    totalBytes: r.metadata.total_file_bytes,
    totalPages: r.metadata.total_pages,
  };
  const pageRange = r.page_range
    ? { startPage: r.page_range.start_page, endPage: r.page_range.end_page }
    : null;
  const raw =
    typeof r.raw_content_b64 === "string" && r.raw_content_b64.length > 0
      ? {
          data: Buffer.from(r.raw_content_b64, "base64") as Uint8Array,
          mime: "application/pdf" as const,
          totalBytes: metadata.totalBytes,
        }
      : undefined;
  const images = r.images.map((img) => {
    const data = Buffer.from(img.content_b64, "base64") as Uint8Array;
    return {
      pageNumber: img.page_number,
      data,
      mime: imageMime(img.format, data, path),
      format: img.format,
      width: img.width,
      height: img.height,
      outputBytes: img.output_bytes,
    };
  });
  const textPages = r.text_pages.map((page) => ({
    pageNumber: page.page_number,
    content: page.content,
  }));

  return {
    kind: "pdf",
    path,
    requestId: result.requestId,
    operation: "read_pdf",
    mode: r.mode,
    metadata,
    pageRange,
    ...(raw ? { raw } : {}),
    images,
    textPages,
    durationMs: result.durationMs,
  };
}

export class CloudClient {
  constructor(private readonly opts: CloudClientOptions) {}

  private fetchImpl(): typeof fetch {
    // Bind to avoid `Illegal invocation` when the caller passes
    // `globalThis.fetch` straight through.
    return this.opts.fetch ?? globalThis.fetch.bind(globalThis);
  }

  /**
   * Dispatch a job and stream its events. Resolves with
   * `{exitCode, durationMs}` on `finished`, rejects with a
   * `CloudClientError` otherwise.
   */
  async spawn(p: SpawnParams): Promise<SpawnResult> {
    // Fast-path: if already aborted before token fetch, skip everything
    if (p.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted before request");
    }

    const fetchImpl = this.fetchImpl();
    const token = await this.opts.getAuthToken();
    // Re-check after async token fetch — signal may have fired during refresh
    if (p.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted after token fetch");
    }

    const requestBody: Record<string, unknown> = {
      deviceId: p.deviceId,
      tool: p.tool,
    };
    if (p.args !== undefined) requestBody.args = p.args;
    if (p.cwd !== undefined) requestBody.cwd = p.cwd;
    if (p.env !== undefined) requestBody.env = p.env;
    if (p.timeoutMs !== undefined) requestBody.timeoutMs = p.timeoutMs;
    if (p.interactive !== undefined) requestBody.interactive = p.interactive;
    if (p.correlationId !== undefined) requestBody.correlationId = p.correlationId;

    let postRes: Response;
    try {
      postRes = await fetchImpl(`${this.opts.hubUrl}/api/control/jobs`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify(requestBody),
        signal: p.signal,
      });
    } catch (err) {
      throw this.normalizeFetchError(err);
    }
    if (!postRes.ok) throw await toTypedHttpError(postRes);

    const postJson = (await postRes.json()) as { jobId?: string };
    const jobId = postJson.jobId;
    if (!jobId) {
      throw new CloudClientError(
        "server_error",
        "Hub response missing jobId",
        { httpStatus: postRes.status },
      );
    }
    if (p.onJobStarted) {
      try {
        p.onJobStarted({ jobId });
      } catch {
        // Swallow — user callback's problem.
      }
    }

    // Subscribe to the SSE stream. From here on, an abort triggers a
    // best-effort cancel before we surface the AbortError.
    const abortHandler = () => {
      // Best-effort: swallow cancel failures — we're already aborting.
      this.cancel(jobId).catch(() => {});
    };
    p.signal?.addEventListener("abort", abortHandler);

    try {
      return await this.streamJob(jobId, token, p);
    } finally {
      p.signal?.removeEventListener("abort", abortHandler);
    }
  }

  /**
   * `POST /api/control/browser`. Single request-response (no SSE):
   * dispatches a browser action via the hub, decodes the snake_case
   * response into camelCase + `Uint8Array` for binary payloads, and
   * resolves with a `BrowserResult`. Errors map to the same taxonomy
   * as `spawn()` (401→`unauthorized`, 504→`timeout`, etc).
   *
   * @deprecated As of 2026-04-30 this method has no live callers. It
   * was used by team9-agent-pi's `AhandBackend.browser()`, which was
   * deleted (team9ai/agent-pi#104) when browser automation migrated
   * to the SKILL model — agents now drive `playwright-cli` via
   * `run_command` against the device's installed binary instead of
   * routing through the hub. The underlying hub endpoint
   * `/api/control/browser` is retained behind a deprecation banner;
   * see `crates/ahand-hub/src/http/browser.rs`. Do NOT add new
   * callers without revisiting that decision; if a future non-
   * playwright backend needs browser dispatch, design the new
   * pathway end-to-end rather than reviving this one.
   */
  async browser(params: BrowserParams): Promise<BrowserResult> {
    if (params.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted before request");
    }

    const fetchImpl = this.fetchImpl();
    const token = await this.opts.getAuthToken();
    if (params.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted after token fetch");
    }

    // Wire format is snake_case. We always send `params: {}` (not
    // omitted) so the hub side always sees an object — matches the
    // schema Task 9 lands. `correlation_id` is sent only when set.
    const requestBody: Record<string, unknown> = {
      device_id: params.deviceId,
      session_id: params.sessionId,
      action: params.action,
      params: params.params ?? {},
      timeout_ms: params.timeoutMs ?? 30_000,
    };
    if (params.correlationId !== undefined) {
      requestBody.correlation_id = params.correlationId;
    }

    let res: Response;
    try {
      res = await fetchImpl(`${this.opts.hubUrl}/api/control/browser`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify(requestBody),
        signal: params.signal,
      });
    } catch (err) {
      throw this.normalizeFetchError(err, params.signal);
    }
    if (!res.ok) throw await toTypedHttpError(res, params.signal);

    let json: {
      success?: boolean;
      data?: unknown;
      error?: string | null;
      binary_data?: string | null;
      binary_mime?: string | null;
      duration_ms?: number;
    };
    try {
      json = (await res.json()) as typeof json;
    } catch (e: unknown) {
      // `res.json()` can fail two ways after the headers/status looked
      // OK: (a) the signal aborted while we were still draining the
      // body, or (b) the body wasn't valid JSON (e.g. an upstream
      // gateway swapped in an HTML 502 page with a 200 status). Both
      // need to surface as proper `CloudClientError`s so callers don't
      // see a raw `SyntaxError` / `AbortError` leaking through.
      if (isAbortError(e)) {
        throw new CloudClientError(
          "abort",
          "browser request aborted during response read",
          { cause: e },
        );
      }
      if (e instanceof SyntaxError) {
        throw new CloudClientError(
          "server_error",
          "Hub returned non-JSON response",
          { cause: e },
        );
      }
      throw new CloudClientError("network", String(e), { cause: e });
    }

    // Strict shape check: a missing or non-boolean `success` field
    // means the hub's wire contract has drifted (e.g. a serializer
    // change emitting `success: 1`). Coercing `=== true` would silently
    // turn every such response into `success: false` with no signal —
    // throw instead so the regression is immediately visible. We also
    // guard the root itself: `JSON.parse("null")` is the literal value
    // `null`, and a misbehaving proxy could return any non-object
    // primitive — accessing `.success` on those would throw a
    // `TypeError` that would escape uncaught.
    if (
      json === null ||
      typeof json !== "object" ||
      typeof (json as { success?: unknown }).success !== "boolean"
    ) {
      throw new CloudClientError(
        "server_error",
        "Hub response malformed: 'success' field missing, not an object, or not a boolean",
      );
    }

    // Decode base64 binary payload into a Uint8Array. `Buffer` is
    // available everywhere this SDK runs (Node + Tauri renderer with
    // node-compat shim) — same assumption already made in
    // `connection.ts`. `Buffer` is a `Uint8Array` subclass, which
    // satisfies the type without an extra copy.
    const binaryB64 = json.binary_data;
    const binary =
      binaryB64 && binaryB64.length > 0
        ? {
            data: Buffer.from(binaryB64, "base64") as Uint8Array,
            mime: json.binary_mime ?? "application/octet-stream",
          }
        : undefined;

    return {
      success: json.success as boolean,
      data: json.data ?? undefined,
      error: json.error ? json.error : undefined,
      binary,
      durationMs: typeof json.duration_ms === "number" ? json.duration_ms : 0,
    };
  }

  /**
   * `POST /api/control/files`. Single request-response: dispatches a
   * file operation via the hub, decodes the snake_case JSON envelope
   * into camelCase, and resolves with a `FileResult`. Errors map to
   * the same taxonomy as `browser()` plus `device_offline` for the
   * 409 case (which the dashboard endpoint also surfaces).
   *
   * Daemon-level errors (NOT_FOUND, POLICY_DENIED, ...) are returned
   * inside the resolved `FileResult` — `success === false` plus an
   * `error` field — rather than thrown as `CloudClientError`. This
   * matches the proto envelope semantics: a successful round-trip is
   * not the same thing as a successful operation.
   */
  async files(params: FileParams): Promise<FileResult> {
    if (params.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted before request");
    }

    const fetchImpl = this.fetchImpl();
    const token = await this.opts.getAuthToken();
    if (params.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted after token fetch");
    }

    // Wire format is snake_case. Always send `params: {}` (not omitted)
    // so the hub side always sees an object — same convention browser()
    // follows. `correlation_id` is sent only when set.
    const requestBody: Record<string, unknown> = {
      device_id: params.deviceId,
      operation: params.operation,
      params: params.params ?? {},
    };
    if (params.timeoutMs !== undefined) {
      requestBody.timeout_ms = params.timeoutMs;
    }
    if (params.correlationId !== undefined) {
      requestBody.correlation_id = params.correlationId;
    }

    let res: Response;
    try {
      res = await fetchImpl(`${this.opts.hubUrl}/api/control/files`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify(requestBody),
        signal: params.signal,
      });
    } catch (err) {
      throw this.normalizeFetchError(err, params.signal);
    }
    if (!res.ok) throw await toTypedHttpError(res, params.signal);

    let json: {
      request_id?: string;
      operation?: string;
      success?: boolean;
      result?: unknown;
      error?: { code?: string; message?: string; path?: string } | null;
      duration_ms?: number;
    };
    try {
      json = (await res.json()) as typeof json;
    } catch (e: unknown) {
      // Same defensive read as `browser()`: aborted-mid-body and
      // non-JSON responses both need to surface as proper
      // `CloudClientError`s.
      if (isAbortError(e)) {
        throw new CloudClientError(
          "abort",
          "files request aborted during response read",
          { cause: e },
        );
      }
      if (e instanceof SyntaxError) {
        throw new CloudClientError(
          "server_error",
          "Hub returned non-JSON response",
          { cause: e },
        );
      }
      throw new CloudClientError("network", String(e), { cause: e });
    }

    // Strict shape check — same rationale as `browser()`. A non-object
    // root or missing `success` field means the hub's wire contract has
    // drifted; coercing would silently mask the regression.
    if (
      json === null ||
      typeof json !== "object" ||
      Array.isArray(json) ||
      typeof (json as { success?: unknown }).success !== "boolean"
    ) {
      throw new CloudClientError(
        "server_error",
        "Hub response malformed: 'success' field missing, not an object, or not a boolean",
      );
    }

    const error =
      json.error && typeof json.error === "object"
        ? {
            code:
              typeof json.error.code === "string" ? json.error.code : "unspecified",
            message:
              typeof json.error.message === "string" ? json.error.message : "",
            path: typeof json.error.path === "string" ? json.error.path : "",
          }
        : undefined;

    return {
      requestId: typeof json.request_id === "string" ? json.request_id : "",
      operation:
        typeof json.operation === "string" ? json.operation : params.operation,
      success: json.success as boolean,
      // `result` is the daemon's per-op payload — pass through as
      // `unknown`. Callers should cast based on `params.operation`.
      result: json.result === null ? undefined : json.result,
      error,
      durationMs: typeof json.duration_ms === "number" ? json.duration_ms : 0,
    };
  }

  /**
   * `POST /api/control/files/upload-url`. Requests a control-plane
   * object-storage upload URL for the given device and decodes the
   * snake_case hub response into camelCase.
   */
  async createFileUploadUrl(
    params: FileUploadUrlParams,
  ): Promise<FileUploadUrlResult> {
    if (params.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted before request");
    }

    const fetchImpl = this.fetchImpl();
    const token = await this.opts.getAuthToken();
    if (params.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted after token fetch");
    }

    let res: Response;
    try {
      res = await fetchImpl(`${this.opts.hubUrl}/api/control/files/upload-url`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({ device_id: params.deviceId }),
        signal: params.signal,
      });
    } catch (err) {
      throw this.normalizeFetchError(err, params.signal);
    }
    if (!res.ok) throw await toTypedHttpError(res, params.signal);

    let json: {
      object_key?: unknown;
      upload_url?: unknown;
      expires_at_ms?: unknown;
    };
    try {
      json = (await res.json()) as typeof json;
    } catch (e: unknown) {
      if (isAbortError(e)) {
        throw new CloudClientError(
          "abort",
          "upload-url request aborted during response read",
          { cause: e },
        );
      }
      if (e instanceof SyntaxError) {
        throw new CloudClientError(
          "server_error",
          "Hub returned non-JSON response",
          { cause: e },
        );
      }
      throw this.normalizeFetchError(e, params.signal);
    }

    if (
      json === null ||
      typeof json !== "object" ||
      Array.isArray(json) ||
      typeof json.object_key !== "string" ||
      typeof json.upload_url !== "string" ||
      typeof json.expires_at_ms !== "number"
    ) {
      throw new CloudClientError(
        "server_error",
        "Hub response malformed: expected object_key, upload_url, and expires_at_ms",
      );
    }

    return {
      objectKey: json.object_key,
      uploadUrl: json.upload_url,
      expiresAtMs: json.expires_at_ms,
    };
  }

  /**
   * High-level file read helper that chooses the right ahand file op and
   * returns a typed, decoded result. `mode: "auto"` is intentionally owned by
   * the SDK so consumers don't need to know whether a path should go through
   * `read_text`, `read_image`, or `read_binary`.
   *
   * Auto routing today:
   * - image-looking paths (`.png`, `.jpg`, `.webp`, etc.) -> `read_image`
   * - `.pdf` paths -> `read_pdf` (metadata + first 5 pages by default)
   * - everything else -> `read_text`, falling back to `read_binary` only when
   *   the daemon reports a text encoding failure
   */
  async readFile(params: ReadFileParams): Promise<ReadFileResult> {
    const mode = params.mode ?? "auto";
    switch (mode) {
      case "text":
        return this.readTextFile(params);
      case "image":
        return this.readImageFile(params);
      case "binary":
        return this.readBinaryFile(params);
      case "auto":
        if (IMAGE_PATH_RE.test(params.path)) {
          return this.readImageFile(params);
        }
        if (PDF_PATH_RE.test(params.path)) {
          return this.readPdf({
            deviceId: params.deviceId,
            path: params.path,
            mode: "auto",
            noFollowSymlink: params.noFollowSymlink,
            timeoutMs: params.timeoutMs,
            correlationId: params.correlationId,
            signal: params.signal,
          });
        }
        return this.readAutoTextThenBinary(params);
      default:
        throw new CloudClientError(
          "bad_request",
          `Unsupported readFile mode: ${String(mode)}`,
        );
    }
  }

  async readPdf(params: ReadPdfParams): Promise<ReadFilePdfResult> {
    const result = await this.files({
      deviceId: params.deviceId,
      operation: "read_pdf",
      params: buildReadPdfParams(params),
      timeoutMs: params.timeoutMs,
      correlationId: params.correlationId,
      signal: params.signal,
    });
    if (!result.success) throwFileResultError("read_pdf", result);
    return normalizePdfReadResult(params.path, result);
  }

  private async readAutoTextThenBinary(
    params: ReadFileParams,
  ): Promise<ReadFileResult> {
    const textResult = await this.dispatchReadText(params);
    if (textResult.success) {
      return normalizeTextReadResult(params.path, textResult);
    }
    if (textResult.error?.code === "encoding") {
      return this.readBinaryFile(params);
    }
    throwFileResultError("read_text", textResult);
  }

  private async readTextFile(params: ReadFileParams): Promise<ReadFileTextResult> {
    const result = await this.dispatchReadText(params);
    if (!result.success) throwFileResultError("read_text", result);
    return normalizeTextReadResult(params.path, result);
  }

  private async dispatchReadText(params: ReadFileParams): Promise<FileResult> {
    return this.files({
      deviceId: params.deviceId,
      operation: "read_text",
      params: buildReadTextParams(params),
      timeoutMs: params.timeoutMs,
      correlationId: params.correlationId,
      signal: params.signal,
    });
  }

  private async readBinaryFile(
    params: ReadFileParams,
  ): Promise<ReadFileBinaryResult> {
    const result = await this.files({
      deviceId: params.deviceId,
      operation: "read_binary",
      params: buildReadBinaryParams(params),
      timeoutMs: params.timeoutMs,
      correlationId: params.correlationId,
      signal: params.signal,
    });
    if (!result.success) throwFileResultError("read_binary", result);
    const r = requireReadBinaryWireResult(result.result);
    const data = await this.readWireBytes(
      "read_binary",
      r.content_b64,
      r.download_url,
      params.signal,
    );
    return {
      kind: "binary",
      path: params.path,
      requestId: result.requestId,
      operation: "read_binary",
      data,
      mime: mimeFromPath(params.path),
      byteOffset: r.byte_offset,
      bytesRead: r.bytes_read,
      totalBytes: r.total_file_bytes,
      remainingBytes: r.remaining_bytes,
      durationMs: result.durationMs,
    };
  }

  private async readImageFile(params: ReadFileParams): Promise<ReadFileImageResult> {
    const result = await this.files({
      deviceId: params.deviceId,
      operation: "read_image",
      params: buildReadImageParams(params),
      timeoutMs: params.timeoutMs,
      correlationId: params.correlationId,
      signal: params.signal,
    });
    if (!result.success) throwFileResultError("read_image", result);
    const r = requireReadImageWireResult(result.result);
    const data = await this.readWireBytes(
      "read_image",
      r.content_b64,
      r.download_url,
      params.signal,
    );
    return {
      kind: "image",
      path: params.path,
      requestId: result.requestId,
      operation: "read_image",
      data,
      mime: imageMime(r.format, data, params.path),
      format: r.format,
      width: r.width,
      height: r.height,
      originalBytes: r.original_bytes,
      outputBytes: r.output_bytes,
      durationMs: result.durationMs,
    };
  }

  private async readWireBytes(
    operation: "read_binary" | "read_image",
    contentB64: string | undefined,
    downloadUrl: string | null | undefined,
    signal: AbortSignal | undefined,
  ): Promise<Uint8Array> {
    if (typeof downloadUrl === "string" && downloadUrl.length > 0) {
      let res: Response;
      try {
        res = await this.fetchImpl()(downloadUrl, { signal });
      } catch (err) {
        throw this.normalizeFetchError(err);
      }
      if (!res.ok) {
        throw new CloudClientError(
          "server_error",
          `${operation} download_url fetch failed: ${res.status} ${res.statusText}`.trim(),
          { httpStatus: res.status },
        );
      }
      try {
        return new Uint8Array(await res.arrayBuffer());
      } catch (err) {
        throw this.normalizeFetchError(err);
      }
    }
    if (typeof contentB64 !== "string") {
      throw malformedFilePayload(operation, "missing content_b64 and download_url");
    }
    return Buffer.from(contentB64, "base64") as Uint8Array;
  }

  /** `POST /api/control/jobs/{id}/cancel`. Returns 202 with no body. */
  async cancel(jobId: string): Promise<void> {
    const fetchImpl = this.fetchImpl();
    const token = await this.opts.getAuthToken();
    let res: Response;
    try {
      res = await fetchImpl(
        `${this.opts.hubUrl}/api/control/jobs/${encodeURIComponent(jobId)}/cancel`,
        {
          method: "POST",
          headers: { Authorization: `Bearer ${token}` },
        },
      );
    } catch (err) {
      throw this.normalizeFetchError(err);
    }
    if (!res.ok) throw await toTypedHttpError(res);
  }

  /**
   * `GET /api/devices/{deviceId}/app-tools`. Returns the catalog of
   * application-defined tools registered by the host app embedding
   * `ahandd` on the target device. Hub-level failures (auth, not found,
   * timeout) throw `CloudClientError`; use the code to discriminate.
   *
   * The hub returns camelCase JSON matching `AppToolCatalog` directly.
   * A minimal shape check (root must be an object with a numeric
   * `revision` and an `Array` `tools`) is applied defensively to catch
   * wire-contract drift early.
   */
  async listAppTools(
    deviceId: string,
    opts?: ListAppToolsOptions,
  ): Promise<AppToolCatalog> {
    if (opts?.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted before request");
    }

    const fetchImpl = this.fetchImpl();
    const token = await this.opts.getAuthToken();
    if (opts?.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted after token fetch");
    }

    let res: Response;
    try {
      res = await fetchImpl(
        `${this.opts.hubUrl}/api/devices/${encodeURIComponent(deviceId)}/app-tools`,
        {
          method: "GET",
          headers: {
            Authorization: `Bearer ${token}`,
          },
          signal: opts?.signal,
        },
      );
    } catch (err) {
      throw this.normalizeFetchError(err, opts?.signal);
    }
    if (!res.ok) throw await toTypedHttpError(res, opts?.signal);

    let json: unknown;
    try {
      json = await res.json();
    } catch (e: unknown) {
      if (isAbortError(e)) {
        throw new CloudClientError(
          "abort",
          "listAppTools request aborted during response read",
          { cause: e },
        );
      }
      if (e instanceof SyntaxError) {
        throw new CloudClientError(
          "server_error",
          "Hub returned non-JSON response",
          { cause: e },
        );
      }
      throw new CloudClientError("network", String(e), { cause: e });
    }

    // Defensive shape check: root must be an object with a numeric `revision`
    // and an array `tools`. Same coercion-masks-regression rationale as files().
    if (
      json === null ||
      typeof json !== "object" ||
      Array.isArray(json) ||
      typeof (json as { revision?: unknown }).revision !== "number" ||
      !Array.isArray((json as { tools?: unknown }).tools)
    ) {
      throw new CloudClientError(
        "server_error",
        "Hub response malformed: expected AppToolCatalog with numeric revision and tools array",
      );
    }

    return json as AppToolCatalog;
  }

  /**
   * `POST /api/control/app-tool`. Invokes an application-defined tool on
   * the target device and waits for the result. The hub forwards the call
   * to the daemon and blocks until the tool completes or the timeout
   * fires.
   *
   * Hub-level errors (auth, offline, timeout) throw `CloudClientError`.
   * Daemon-level failures surface as `CloudClientError("app_tool_error")`
   * with the daemon error code in `jobErrorCode`. The daemon error code
   * set is: `TOOL_NOT_FOUND | INVALID_ARGS | SESSION_INACTIVE |
   * APPROVAL_DENIED | APPROVAL_TIMEOUT | EXECUTION_TIMEOUT |
   * HANDLER_PANIC | HANDLER_ERROR | CONCURRENCY_LIMIT`.
   * See the proto `AppToolErrorCode` enum for the authoritative list.
   *
   * Returns `body.result` on success. When the daemon returns a
   * successful response but omits the `result` field, `null` is returned
   * for a stable contract (callers can always do `result ?? default`).
   */
  async invokeAppTool(
    deviceId: string,
    name: string,
    args?: Record<string, unknown>,
    opts?: InvokeAppToolOptions,
  ): Promise<unknown> {
    if (opts?.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted before request");
    }

    const fetchImpl = this.fetchImpl();
    const token = await this.opts.getAuthToken();
    if (opts?.signal?.aborted) {
      throw new CloudClientError("abort", "Aborted after token fetch");
    }

    // camelCase body; omit undefined optional fields
    const requestBody: Record<string, unknown> = { deviceId, name };
    if (args !== undefined) requestBody.args = args;
    if (opts?.timeoutMs !== undefined) requestBody.timeoutMs = opts.timeoutMs;

    let res: Response;
    try {
      res = await fetchImpl(`${this.opts.hubUrl}/api/control/app-tool`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify(requestBody),
        signal: opts?.signal,
      });
    } catch (err) {
      throw this.normalizeFetchError(err, opts?.signal);
    }
    if (!res.ok) throw await toTypedHttpError(res, opts?.signal);

    let json: unknown;
    try {
      json = await res.json();
    } catch (e: unknown) {
      if (isAbortError(e)) {
        throw new CloudClientError(
          "abort",
          "invokeAppTool request aborted during response read",
          { cause: e },
        );
      }
      if (e instanceof SyntaxError) {
        throw new CloudClientError(
          "server_error",
          "Hub returned non-JSON response",
          { cause: e },
        );
      }
      throw new CloudClientError("network", String(e), { cause: e });
    }

    // Strict shape check — same rationale as `files()`. A non-object root
    // (null, array, primitive) means the hub's wire contract has drifted;
    // coercing would silently mask the regression.
    if (json === null || typeof json !== "object" || Array.isArray(json)) {
      throw new CloudClientError(
        "server_error",
        "Hub response malformed: expected invokeAppTool object response",
      );
    }

    const typedJson = json as {
      toolCallId?: string;
      result?: unknown;
      error?: { code?: string; message?: string } | null;
    };

    // A 200 with an `error` field is a daemon-level failure.
    if (typedJson.error != null) {
      throw new CloudClientError(
        "app_tool_error",
        typedJson.error.message ?? "app tool failed",
        {
          jobErrorCode: typedJson.error.code,
          jobErrorMessage: typedJson.error.message,
        },
      );
    }

    // Return `null` when `result` is missing for a stable contract.
    return typedJson.result !== undefined ? typedJson.result : null;
  }

  /**
   * Subscribe to `/stream` and dispatch events to callbacks until a
   * terminal `finished` / `error` event — or the stream ends
   * prematurely, which is treated as `stream_ended`.
   */
  private async streamJob(
    jobId: string,
    token: string,
    p: SpawnParams,
  ): Promise<SpawnResult> {
    const fetchImpl = this.fetchImpl();
    let streamRes: Response;
    try {
      streamRes = await fetchImpl(
        `${this.opts.hubUrl}/api/control/jobs/${encodeURIComponent(jobId)}/stream`,
        {
          headers: {
            Authorization: `Bearer ${token}`,
            Accept: "text/event-stream",
          },
          signal: p.signal,
        },
      );
    } catch (err) {
      throw this.normalizeFetchError(err);
    }
    if (!streamRes.ok) throw await toTypedHttpError(streamRes);
    if (!streamRes.body) {
      throw new CloudClientError(
        "server_error",
        "SSE response had no body",
        { httpStatus: streamRes.status },
      );
    }

    const reader = streamRes.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";

    try {
      while (true) {
        let chunk: ReadableStreamReadResult<Uint8Array>;
        try {
          chunk = await reader.read();
        } catch (err) {
          throw this.normalizeFetchError(err);
        }
        const { done, value } = chunk;
        if (done) {
          // Prefer reporting abort over stream_ended so the caller gets
          // the right error code when the signal fired and the server
          // closed the stream as a result.
          if (p.signal?.aborted) {
            throw new CloudClientError("abort", "Aborted", { cause: p.signal.reason });
          }
          throw new CloudClientError(
            "stream_ended",
            "SSE stream ended before finished event",
          );
        }
        buf += decoder.decode(value, { stream: true });

        let sep = findEventBoundary(buf);
        while (sep !== -1) {
          const rawEvent = buf.slice(0, sep.start);
          buf = buf.slice(sep.end);

          // Pure comment blocks (keepalives) are dropped silently.
          if (isSseComment(rawEvent)) {
            sep = findEventBoundary(buf);
            continue;
          }

          const parsed = parseSseEvent(rawEvent);
          const result = this.dispatchEvent(parsed, p);
          if (result) return result;
          sep = findEventBoundary(buf);
        }
      }
    } finally {
      // `cancel()` here is the ReadableStreamDefaultReader method — it
      // releases the lock and signals the server we're no longer
      // reading. Safe to call even after the stream ended.
      try {
        await reader.cancel();
      } catch {
        // Ignored — we've already raised the outer error.
      }
    }
  }

  /**
   * Dispatch a parsed SSE event to the appropriate callback. Returns
   * a `SpawnResult` for the terminal `finished` event; throws for
   * `error`; returns undefined for all other (including unknown)
   * events so the caller keeps reading.
   *
   * Callback-thrown errors are swallowed for `stdout` / `stderr` /
   * `progress` (per plan acceptance criteria — user bugs shouldn't
   * abort the stream). For `error` events, the thrown error
   * propagates: the stream is already terminal, so the caller needs
   * to know.
   */
  private dispatchEvent(
    ev: ParsedSseEvent,
    p: SpawnParams,
  ): SpawnResult | undefined {
    switch (ev.event) {
      case "stdout": {
        const chunk = safeJsonField<string>(ev.data, "chunk");
        if (chunk !== undefined && p.onStdout) {
          try {
            p.onStdout(chunk);
          } catch {
            // Swallow — user callback's problem.
          }
        }
        return undefined;
      }
      case "stderr": {
        const chunk = safeJsonField<string>(ev.data, "chunk");
        if (chunk !== undefined && p.onStderr) {
          try {
            p.onStderr(chunk);
          } catch {
            // Swallow.
          }
        }
        return undefined;
      }
      case "progress": {
        const parsed = safeJsonObject(ev.data);
        if (parsed && typeof parsed.percent === "number" && p.onProgress) {
          try {
            p.onProgress({
              percent: parsed.percent,
              message:
                typeof parsed.message === "string" ? parsed.message : undefined,
            });
          } catch {
            // Swallow.
          }
        }
        return undefined;
      }
      case "finished": {
        const parsed = safeJsonObject(ev.data);
        const exitCode =
          parsed && typeof parsed.exitCode === "number" ? parsed.exitCode : 0;
        const durationMs =
          parsed && typeof parsed.durationMs === "number" ? parsed.durationMs : 0;
        return { exitCode, durationMs };
      }
      case "error": {
        const parsed = safeJsonObject(ev.data) ?? {};
        const code = typeof parsed.code === "string" ? parsed.code : "unknown";
        const message =
          typeof parsed.message === "string"
            ? parsed.message
            : "Hub reported job error";
        throw new CloudClientError("job_error", message, {
          jobErrorCode: code,
          jobErrorMessage: message,
        });
      }
      default:
        // Unknown event names are silently ignored — forward
        // compatibility with future hub events.
        return undefined;
    }
  }

  /**
   * Turn a fetch-thrown error into a `CloudClientError`. Abort signals
   * surface as `abort`; everything else as `network` with the original
   * error attached as `cause`.
   */
  private normalizeFetchError(
    err: unknown,
    signal?: AbortSignal,
  ): CloudClientError {
    if (isAbortError(err) || signal?.aborted) {
      return new CloudClientError("abort", "Aborted", { cause: err });
    }
    const message = err instanceof Error ? err.message : String(err);
    return new CloudClientError("network", message || "Network error", {
      cause: err,
    });
  }
}

/** Detect whether `err` is a DOMException / fetch AbortError. */
function isAbortError(err: unknown): boolean {
  if (!err || typeof err !== "object") return false;
  const name = (err as { name?: unknown }).name;
  return name === "AbortError";
}

/**
 * Find the next SSE event boundary (`\n\n` or `\r\n\r\n`) inside `buf`.
 * Returns the boundary's start + length (end = start + boundary.length)
 * or `-1` if none found. Using both separators handles proxies that
 * rewrite line endings.
 */
function findEventBoundary(
  buf: string,
): { start: number; end: number } | -1 {
  const lfIdx = buf.indexOf("\n\n");
  const crlfIdx = buf.indexOf("\r\n\r\n");
  if (lfIdx === -1 && crlfIdx === -1) return -1;
  // Pick the earliest boundary, so chunks that mix `\n\n` and
  // `\r\n\r\n` split deterministically.
  if (crlfIdx === -1 || (lfIdx !== -1 && lfIdx < crlfIdx)) {
    return { start: lfIdx, end: lfIdx + 2 };
  }
  return { start: crlfIdx, end: crlfIdx + 4 };
}

/**
 * Parse `data` as JSON and return the raw object, or `undefined` on
 * parse failure / non-object shape. Malformed events don't take the
 * stream down — they're treated like unknown events and ignored.
 */
function safeJsonObject(data: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(data);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // Fall through.
  }
  return undefined;
}

/** Convenience for `{field: T}` payloads (stdout, stderr). */
function safeJsonField<T>(data: string, field: string): T | undefined {
  const obj = safeJsonObject(data);
  if (!obj) return undefined;
  const value = obj[field];
  return value as T | undefined;
}
