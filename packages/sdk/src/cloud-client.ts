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
  | "timeout";

/**
 * Typed error raised by `CloudClient`. Use `.code` to discriminate,
 * `.httpStatus` for HTTP-sourced errors, and `.jobErrorCode` /
 * `.jobErrorMessage` for SSE `error` events forwarded from the hub.
 */
export class CloudClientError extends Error {
  readonly code: CloudClientErrorCode;
  readonly httpStatus?: number;
  /** Hub's `code` field for `job_error` (SSE error events). */
  readonly jobErrorCode?: string;
  /** Hub's `message` field for `job_error`. */
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
async function toTypedHttpError(res: Response): Promise<CloudClientError> {
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
  } catch {
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
      throw this.normalizeFetchError(err);
    }
    if (!res.ok) throw await toTypedHttpError(res);

    const json = (await res.json()) as {
      success?: boolean;
      data?: unknown;
      error?: string | null;
      binary_data?: string | null;
      binary_mime?: string | null;
      duration_ms?: number;
    };

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
      success: json.success === true,
      data: json.data ?? undefined,
      error: json.error ? json.error : undefined,
      binary,
      durationMs: typeof json.duration_ms === "number" ? json.duration_ms : 0,
    };
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
  private normalizeFetchError(err: unknown): CloudClientError {
    if (isAbortError(err)) {
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
