import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  CloudClient,
  CloudClientError,
  type CloudClientOptions,
} from "./cloud-client.ts";

// ---------------------------------------------------------------------------
// Helpers to build mock fetch implementations.
// ---------------------------------------------------------------------------

type FetchCall = { url: string; init?: RequestInit };

/** Build a mock `fetch` that records calls and returns the given responses in order. */
function mockFetch(responses: (() => Response | Promise<Response>)[]): {
  fn: typeof fetch;
  calls: FetchCall[];
} {
  const calls: FetchCall[] = [];
  let idx = 0;
  const fn = vi.fn(async (url: string | URL, init?: RequestInit) => {
    calls.push({ url: String(url), init });
    const factory = responses[idx++];
    if (!factory) throw new Error(`Unexpected fetch call #${idx} to ${url}`);
    return factory();
  }) as unknown as typeof fetch;
  return { fn, calls };
}

/** Build a `Response` with a fixed JSON body. */
function jsonResponse(
  body: unknown,
  status = 200,
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/** Build a `Response` whose body is an SSE stream from the given event chunks. */
function sseResponse(chunks: string[], status = 200): Response {
  const encoder = new TextEncoder();
  let idx = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      if (idx >= chunks.length) {
        controller.close();
        return;
      }
      controller.enqueue(encoder.encode(chunks[idx++]));
    },
  });
  return new Response(stream, {
    status,
    headers: { "Content-Type": "text/event-stream" },
  });
}

/** Format a single SSE event block. */
function sseEvent(
  event: string,
  data: Record<string, unknown>,
): string {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

/** SSE keepalive comment. */
const sseKeepalive = ": keepalive\n\n";

// Default options used in all tests.
const BASE_OPTS: Omit<CloudClientOptions, "fetch"> = {
  hubUrl: "https://hub.test",
  getAuthToken: async () => "test-token",
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("CloudClient.spawn", () => {
  it("happy path: stdout → stderr → progress → finished", async () => {
    const { fn, calls } = mockFetch([
      // POST /api/control/jobs → 201 {jobId}
      () => jsonResponse({ jobId: "job-001" }, 201),
      // GET /stream → SSE
      () =>
        sseResponse([
          sseEvent("stdout", { chunk: "hello" }),
          sseEvent("stderr", { chunk: "warn" }),
          sseEvent("progress", { percent: 50, message: "halfway" }),
          sseEvent("finished", { exitCode: 0, durationMs: 123 }),
        ]),
    ]);

    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const stdoutChunks: string[] = [];
    const stderrChunks: string[] = [];
    const progressEvents: { percent: number; message?: string }[] = [];

    const result = await client.spawn({
      deviceId: "dev-1",
      tool: "bash",
      args: ["-c", "echo hello"],
      onStdout: (c) => stdoutChunks.push(c),
      onStderr: (c) => stderrChunks.push(c),
      onProgress: (p) => progressEvents.push(p),
    });

    expect(result).toEqual({ exitCode: 0, durationMs: 123 });
    expect(stdoutChunks).toEqual(["hello"]);
    expect(stderrChunks).toEqual(["warn"]);
    expect(progressEvents).toEqual([{ percent: 50, message: "halfway" }]);

    // Verify request shape.
    expect(calls[0].url).toBe("https://hub.test/api/control/jobs");
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body).toMatchObject({ deviceId: "dev-1", tool: "bash", args: ["-c", "echo hello"] });
    expect(calls[1].url).toBe("https://hub.test/api/control/jobs/job-001/stream");
    expect(calls[1].init?.headers).toMatchObject({ Authorization: "Bearer test-token" });
  });

  it("sends optional fields (env, cwd, timeoutMs, correlationId, interactive)", async () => {
    const { fn, calls } = mockFetch([
      () => jsonResponse({ jobId: "job-x" }, 201),
      () => sseResponse([sseEvent("finished", { exitCode: 0, durationMs: 1 })]),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await client.spawn({
      deviceId: "d",
      tool: "t",
      cwd: "/tmp",
      env: { A: "1" },
      timeoutMs: 5000,
      correlationId: "cid-1",
      interactive: true,
    });
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body).toMatchObject({
      cwd: "/tmp",
      env: { A: "1" },
      timeoutMs: 5000,
      correlationId: "cid-1",
      interactive: true,
    });
  });

  it("bad: 401 POST → CloudClientError(unauthorized)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ error: { code: "unauthorized", message: "bad token" } }, 401),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect(err).toBeInstanceOf(CloudClientError);
    expect((err as CloudClientError).code).toBe("unauthorized");
    expect((err as CloudClientError).httpStatus).toBe(401);
  });

  it("bad: 404 POST → CloudClientError(not_found)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 404)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("not_found");
    expect((err as CloudClientError).httpStatus).toBe(404);
  });

  it("bad: 429 POST → CloudClientError(rate_limited)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 429)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("rate_limited");
  });

  it("bad: 400 POST → CloudClientError(bad_request)", async () => {
    const { fn } = mockFetch([() => jsonResponse({ error: { message: "tool empty" } }, 400)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("bad_request");
    expect((err as CloudClientError).message).toBe("tool empty");
  });

  it("bad: SSE ends without finished event → stream_ended", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () => sseResponse([sseEvent("stdout", { chunk: "partial" })]), // stream closes without finished
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("stream_ended");
  });

  it("bad: SSE error event → CloudClientError(job_error)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () => sseResponse([sseEvent("error", { code: "rejected", message: "denied" })]),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("job_error");
    expect((err as CloudClientError).jobErrorCode).toBe("rejected");
    expect((err as CloudClientError).jobErrorMessage).toBe("denied");
  });

  it("bad: getAuthToken throws → rejects with that error", async () => {
    const tokenErr = new Error("refresh failed");
    const client = new CloudClient({
      hubUrl: "https://hub.test",
      getAuthToken: async () => { throw tokenErr; },
    });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect(err).toBe(tokenErr);
  });

  it("bad: abort before POST → no POST, AbortError", async () => {
    const { fn, calls } = mockFetch([]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const ctrl = new AbortController();
    ctrl.abort();
    const err = await client.spawn({ deviceId: "d", tool: "t", signal: ctrl.signal }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("abort");
    expect(calls).toHaveLength(0);
  });

  it("bad: abort mid-SSE → cancel called + AbortError", async () => {
    const ctrl = new AbortController();
    let resolveStream!: () => void;
    const streamPromise = new Promise<void>((res) => (resolveStream = res));

    // POST succeeds immediately; SSE stream stalls (never sends \n\n) until we abort.
    const encoder = new TextEncoder();
    const stream = new ReadableStream<Uint8Array>({
      async start(controller) {
        // Send partial data (no event boundary) to park the reader.
        controller.enqueue(encoder.encode("event: stdout\n"));
        // Wait until the test aborts.
        await streamPromise;
        controller.close();
      },
    });

    let cancelCalled = false;
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j-abort" }, 201),
      () => new Response(stream, { status: 200, headers: { "Content-Type": "text/event-stream" } }),
      () => { cancelCalled = true; return new Response(null, { status: 202 }); },
    ]);

    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const spawnPromise = client.spawn({ deviceId: "d", tool: "t", signal: ctrl.signal });

    // Give spawn time to make the two fetch calls and park on reader.read().
    await new Promise((r) => setTimeout(r, 10));
    ctrl.abort();
    resolveStream();

    const err = await spawnPromise.catch((e) => e);
    expect((err as CloudClientError).code).toBe("abort");
    expect(cancelCalled).toBe(true);
  });

  it("edge: stdout chunk > 1MB across multiple stream chunks → reassembled correctly", async () => {
    const bigPayload = "x".repeat(1_500_000);
    const eventText = sseEvent("stdout", { chunk: bigPayload });
    // Split into 3 pieces.
    const third = Math.floor(eventText.length / 3);
    const pieces = [
      eventText.slice(0, third),
      eventText.slice(third, 2 * third),
      eventText.slice(2 * third),
    ];

    const encoder = new TextEncoder();
    let idx = 0;
    const stream = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (idx < pieces.length) {
          controller.enqueue(encoder.encode(pieces[idx++]));
        } else if (idx === pieces.length) {
          controller.enqueue(encoder.encode(sseEvent("finished", { exitCode: 0, durationMs: 0 })));
          idx++;
        } else {
          controller.close();
        }
      },
    });

    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () => new Response(stream, { status: 200, headers: { "Content-Type": "text/event-stream" } }),
    ]);

    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const chunks: string[] = [];
    await client.spawn({ deviceId: "d", tool: "t", onStdout: (c) => chunks.push(c) });
    expect(chunks).toHaveLength(1);
    expect(chunks[0]).toBe(bigPayload);
  });

  it("edge: data with \\n inside chunk (not \\n\\n) → not mis-split", async () => {
    // A single event whose JSON payload contains literal newlines inside the string.
    const withNewlines = "line1\nline2\nline3";
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () =>
        sseResponse([
          sseEvent("stdout", { chunk: withNewlines }),
          sseEvent("finished", { exitCode: 0, durationMs: 0 }),
        ]),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const chunks: string[] = [];
    await client.spawn({ deviceId: "d", tool: "t", onStdout: (c) => chunks.push(c) });
    expect(chunks).toEqual([withNewlines]);
  });

  it("edge: unknown SSE event type → silently ignored", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () =>
        sseResponse([
          "event: future_event\ndata: {\"surprise\":true}\n\n",
          sseEvent("finished", { exitCode: 0, durationMs: 5 }),
        ]),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const result = await client.spawn({ deviceId: "d", tool: "t" });
    expect(result.exitCode).toBe(0);
  });

  it("edge: keepalive comments → skipped without disturbing state", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () =>
        sseResponse([
          sseKeepalive,
          sseKeepalive,
          sseEvent("stdout", { chunk: "ok" }),
          sseKeepalive,
          sseEvent("finished", { exitCode: 0, durationMs: 1 }),
        ]),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const chunks: string[] = [];
    const result = await client.spawn({ deviceId: "d", tool: "t", onStdout: (c) => chunks.push(c) });
    expect(chunks).toEqual(["ok"]);
    expect(result.exitCode).toBe(0);
  });

  it("edge: callback throws → subsequent chunks still delivered", async () => {
    let callCount = 0;
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () =>
        sseResponse([
          sseEvent("stdout", { chunk: "a" }),
          sseEvent("stdout", { chunk: "b" }),
          sseEvent("stdout", { chunk: "c" }),
          sseEvent("finished", { exitCode: 0, durationMs: 1 }),
        ]),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const received: string[] = [];
    await client.spawn({
      deviceId: "d",
      tool: "t",
      onStdout: (c) => {
        callCount++;
        if (callCount === 1) throw new Error("callback error");
        received.push(c);
      },
    });
    // First chunk threw, but b and c should still be delivered.
    expect(received).toEqual(["b", "c"]);
  });

  it("edge: CRLF line endings in SSE → handled correctly", async () => {
    const crlfEvent =
      "event: stdout\r\ndata: {\"chunk\":\"crlf-test\"}\r\n\r\n" +
      "event: finished\r\ndata: {\"exitCode\":0,\"durationMs\":1}\r\n\r\n";
    const encoder = new TextEncoder();
    const stream = new ReadableStream<Uint8Array>({
      start(c) { c.enqueue(encoder.encode(crlfEvent)); c.close(); },
    });
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () => new Response(stream, { status: 200, headers: { "Content-Type": "text/event-stream" } }),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const chunks: string[] = [];
    await client.spawn({ deviceId: "d", tool: "t", onStdout: (c) => chunks.push(c) });
    expect(chunks).toEqual(["crlf-test"]);
  });

  it("bad: 500 POST → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([() => jsonResponse({ error: { message: "internal" } }, 500)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("server_error");
    expect((err as CloudClientError).httpStatus).toBe(500);
  });

  it("bad: 504 POST → CloudClientError(timeout)", async () => {
    const { fn } = mockFetch([() => jsonResponse({ error: { message: "request timeout" } }, 504)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("timeout");
    expect((err as CloudClientError).httpStatus).toBe(504);
    expect((err as CloudClientError).message).toBe("request timeout");
  });

  it("bad: fetch throws network error → CloudClientError(network)", async () => {
    const netErr = new Error("ECONNREFUSED");
    const { fn } = mockFetch([() => { throw netErr; }]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.spawn({ deviceId: "d", tool: "t" }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("network");
    expect((err as CloudClientError).cause).toBe(netErr);
  });

  it("bad: abort during getAuthToken → CloudClientError(abort)", async () => {
    const ctrl = new AbortController();
    const client = new CloudClient({
      hubUrl: "https://hub.test",
      getAuthToken: async () => {
        ctrl.abort(); // abort fires during token refresh
        return "token"; // still returns a token
      },
    });
    const err = await client.spawn({ deviceId: "d", tool: "t", signal: ctrl.signal }).catch((e) => e);
    expect((err as CloudClientError).code).toBe("abort");
  });

  it("edge: spurious blank-line chunk between events does not swallow subsequent event", async () => {
    // Simulate a proxy injecting extra blank lines before the finished event
    const encoder = new TextEncoder();
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(encoder.encode(sseEvent("stdout", { chunk: "hi" })));
        // Extra blank lines (not a proper SSE event boundary)
        c.enqueue(encoder.encode("\n\n\n\n"));
        c.enqueue(encoder.encode(sseEvent("finished", { exitCode: 0, durationMs: 1 })));
        c.close();
      },
    });
    const { fn } = mockFetch([
      () => jsonResponse({ jobId: "j" }, 201),
      () => new Response(stream, { status: 200, headers: { "Content-Type": "text/event-stream" } }),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const chunks: string[] = [];
    const result = await client.spawn({ deviceId: "d", tool: "t", onStdout: (c) => chunks.push(c) });
    expect(chunks).toEqual(["hi"]);
    expect(result.exitCode).toBe(0);
  });
});

describe("CloudClient.cancel", () => {
  it("happy: POSTs cancel endpoint and resolves", async () => {
    const { fn, calls } = mockFetch([() => new Response(null, { status: 202 })]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(client.cancel("job-99")).resolves.toBeUndefined();
    expect(calls[0].url).toBe("https://hub.test/api/control/jobs/job-99/cancel");
    expect(calls[0].init?.method).toBe("POST");
    expect(calls[0].init?.headers).toMatchObject({ Authorization: "Bearer test-token" });
  });

  it("bad: 404 from cancel → CloudClientError(not_found)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 404)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client.cancel("j").catch((e) => e);
    expect((err as CloudClientError).code).toBe("not_found");
  });

  it("encodes job ID in URL", async () => {
    const { fn, calls } = mockFetch([() => new Response(null, { status: 202 })]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await client.cancel("job/with/slash");
    expect(calls[0].url).toContain(encodeURIComponent("job/with/slash"));
  });
});

describe("CloudClient.browser", () => {
  it("happy: POSTs /api/control/browser with snake_case body + Bearer auth", async () => {
    const { fn, calls } = mockFetch([
      () =>
        jsonResponse(
          { success: true, data: { ok: true }, duration_ms: 12 },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });

    const result = await client.browser({
      deviceId: "d1",
      sessionId: "browser-sess",
      action: "click",
      params: { ref: "e7" },
      timeoutMs: 25_000,
      correlationId: "c-1",
    });

    expect(calls[0].url).toBe("https://hub.test/api/control/browser");
    expect(calls[0].init?.method).toBe("POST");
    expect(calls[0].init?.headers).toMatchObject({
      Authorization: "Bearer test-token",
      "Content-Type": "application/json",
    });
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body).toEqual({
      device_id: "d1",
      session_id: "browser-sess",
      action: "click",
      params: { ref: "e7" },
      timeout_ms: 25_000,
      correlation_id: "c-1",
    });
    expect(result).toEqual({
      success: true,
      data: { ok: true },
      error: undefined,
      binary: undefined,
      durationMs: 12,
    });
  });

  it("happy: decodes binary_data (base64) into Uint8Array with binary_mime", async () => {
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
    const b64 = Buffer.from(png).toString("base64");
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          {
            success: true,
            data: null,
            binary_data: b64,
            binary_mime: "image/png",
            duration_ms: 8,
          },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const r = await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "screenshot",
    });
    expect(r.success).toBe(true);
    expect(r.binary).toBeDefined();
    expect(r.binary?.mime).toBe("image/png");
    expect(Array.from(r.binary!.data)).toEqual([0x89, 0x50, 0x4e, 0x47]);
    expect(r.binary!.data).toBeInstanceOf(Uint8Array);
    // Falsy `data` (null on the wire) collapses to `undefined`.
    expect(r.data).toBeUndefined();
    expect(r.durationMs).toBe(8);
  });

  it("happy: binary_data missing → result.binary is undefined", async () => {
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          { success: true, data: { x: 1 }, duration_ms: 5 },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const r = await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "snapshot",
    });
    expect(r.binary).toBeUndefined();
    expect(r.data).toEqual({ x: 1 });
  });

  it("happy: binary_data empty string → result.binary is undefined", async () => {
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          {
            success: true,
            data: null,
            binary_data: "",
            binary_mime: "image/png",
            duration_ms: 1,
          },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const r = await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "screenshot",
    });
    expect(r.binary).toBeUndefined();
  });

  it("happy: binary_mime absent defaults to application/octet-stream", async () => {
    const bytes = new Uint8Array([1, 2, 3]);
    const b64 = Buffer.from(bytes).toString("base64");
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          { success: true, binary_data: b64, duration_ms: 2 },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const r = await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "download",
    });
    expect(r.binary?.mime).toBe("application/octet-stream");
    expect(Array.from(r.binary!.data)).toEqual([1, 2, 3]);
  });

  it("happy: defaults timeout_ms to 30000 when not provided", async () => {
    const { fn, calls } = mockFetch([
      () => jsonResponse({ success: true, duration_ms: 1 }, 200),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "snapshot",
    });
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body.timeout_ms).toBe(30_000);
  });

  it("happy: omitted params field serializes as empty object {}", async () => {
    const { fn, calls } = mockFetch([
      () => jsonResponse({ success: true, duration_ms: 1 }, 200),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "snapshot",
    });
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body.params).toEqual({});
    // correlation_id should NOT be present on the wire when caller didn't set it.
    expect("correlation_id" in body).toBe(false);
  });

  it("happy: surfaces hub-supplied error string and success=false", async () => {
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          {
            success: false,
            data: null,
            error: "navigation failed",
            duration_ms: 99,
          },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const r = await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "open",
      params: { url: "x" },
    });
    expect(r.success).toBe(false);
    expect(r.error).toBe("navigation failed");
    expect(r.durationMs).toBe(99);
  });

  it("bad: HTTP 401 → CloudClientError(unauthorized)", async () => {
    const { fn } = mockFetch([
      () =>
        jsonResponse({ error: { message: "no token" } }, 401),
    ]);
    const client = new CloudClient({
      hubUrl: "https://hub.test",
      getAuthToken: async () => "",
      fetch: fn,
    });
    const err = await client
      .browser({
        deviceId: "d",
        sessionId: "s",
        action: "open",
        params: { url: "x" },
      })
      .catch((e) => e);
    expect(err).toBeInstanceOf(CloudClientError);
    expect((err as CloudClientError).code).toBe("unauthorized");
    expect((err as CloudClientError).httpStatus).toBe(401);
  });

  it("bad: HTTP 403 → CloudClientError(forbidden)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 403)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("forbidden");
  });

  it("bad: HTTP 404 → CloudClientError(not_found)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 404)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("not_found");
  });

  it("bad: HTTP 429 → CloudClientError(rate_limited)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 429)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("rate_limited");
  });

  it("bad: HTTP 504 → CloudClientError(timeout)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ error: { message: "timed out" } }, 504),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("timeout");
    expect((err as CloudClientError).httpStatus).toBe(504);
  });

  it("bad: HTTP 500 → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 500)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("server_error");
  });

  it("bad: fetch throws (network) → CloudClientError(network) with cause", async () => {
    const cause = new Error("ECONNREFUSED");
    const fn = (async () => {
      throw cause;
    }) as unknown as typeof fetch;
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("network");
    expect((err as CloudClientError).cause).toBe(cause);
  });

  it("bad: getAuthToken throws → propagates the original error", async () => {
    const tokenErr = new Error("refresh failed");
    const client = new CloudClient({
      hubUrl: "https://hub.test",
      getAuthToken: async () => {
        throw tokenErr;
      },
    });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect(err).toBe(tokenErr);
  });

  it("bad: abort before request → no fetch call, code=abort", async () => {
    const { fn, calls } = mockFetch([]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const ctrl = new AbortController();
    ctrl.abort();
    const err = await client
      .browser({
        deviceId: "d",
        sessionId: "s",
        action: "snapshot",
        signal: ctrl.signal,
      })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("abort");
    expect(calls).toHaveLength(0);
  });

  it("bad: abort during fetch → CloudClientError(abort)", async () => {
    const ctrl = new AbortController();
    const fn = (async (_url: string | URL, init?: RequestInit) => {
      await new Promise<never>((_, reject) => {
        const onAbort = () => {
          const ev = new Error("aborted");
          (ev as Error & { name: string }).name = "AbortError";
          reject(ev);
        };
        if (init?.signal?.aborted) onAbort();
        else init?.signal?.addEventListener("abort", onAbort);
      });
      // Unreachable.
      return new Response(null);
    }) as unknown as typeof fetch;
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const promise = client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "snapshot",
      signal: ctrl.signal,
    });
    setTimeout(() => ctrl.abort(), 5);
    const err = await promise.catch((e) => e);
    expect((err as CloudClientError).code).toBe("abort");
  });

  it("bad: getAuthToken is called once per request", async () => {
    let calls = 0;
    const { fn } = mockFetch([
      () => jsonResponse({ success: true, duration_ms: 1 }, 200),
    ]);
    const client = new CloudClient({
      hubUrl: "https://hub.test",
      getAuthToken: async () => {
        calls++;
        return "tok";
      },
      fetch: fn,
    });
    await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "snapshot",
    });
    expect(calls).toBe(1);
  });

  it("bad: abort during getAuthToken → no fetch call, code=abort", async () => {
    // Regression guard for the post-token-fetch abort fast-path. If
    // someone removes the second `signal?.aborted` check, an abort that
    // fires while the token is being refreshed would still proceed to
    // call fetch — this test catches that.
    let fetchCalls = 0;
    const fn = (async () => {
      fetchCalls++;
      return jsonResponse({ success: true, duration_ms: 1 }, 200);
    }) as unknown as typeof fetch;
    const ctrl = new AbortController();
    const client = new CloudClient({
      hubUrl: "https://hub.test",
      getAuthToken: async () => {
        // Simulate a slow token refresh; fire abort while we wait.
        await new Promise((r) => setTimeout(r, 10));
        return "tok";
      },
      fetch: fn,
    });
    const promise = client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "snapshot",
      signal: ctrl.signal,
    });
    // Abort while getAuthToken is still pending.
    setTimeout(() => ctrl.abort(), 1);
    const err = await promise.catch((e) => e);
    expect((err as CloudClientError).code).toBe("abort");
    expect(fetchCalls).toBe(0);
  });

  it("bad: response missing 'success' field → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ duration_ms: 1 }, 200),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.browser({ deviceId: "d", sessionId: "s", action: "snapshot" }),
    ).rejects.toMatchObject({ code: "server_error" });
  });

  it("bad: response with non-boolean 'success' (e.g. 1) → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ success: 1, duration_ms: 1 }, 200),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.browser({ deviceId: "d", sessionId: "s", action: "snapshot" }),
    ).rejects.toMatchObject({ code: "server_error" });
  });

  it("bad: HTTP 400 → CloudClientError(bad_request)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ error: { message: "invalid params" } }, 400),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect(err).toBeInstanceOf(CloudClientError);
    expect((err as CloudClientError).code).toBe("bad_request");
    expect((err as CloudClientError).httpStatus).toBe(400);
  });

  it("bad: HTTP 502 → CloudClientError(server_error) (non-mapped 5xx)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 502)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("server_error");
    expect((err as CloudClientError).httpStatus).toBe(502);
  });

  it("bad: HTTP 503 → CloudClientError(server_error) (non-mapped 5xx)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 503)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("server_error");
    expect((err as CloudClientError).httpStatus).toBe(503);
  });

  it("bad: HTTP 418 → CloudClientError(bad_request) (non-mapped 4xx)", async () => {
    const { fn } = mockFetch([() => jsonResponse({}, 418)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const err = await client
      .browser({ deviceId: "d", sessionId: "s", action: "snapshot" })
      .catch((e) => e);
    expect((err as CloudClientError).code).toBe("bad_request");
    expect((err as CloudClientError).httpStatus).toBe(418);
  });

  it("bad: 200 with non-JSON body → CloudClientError(server_error)", async () => {
    // Upstream gateway can swap an HTML 502 page in front of a 200 status.
    // `res.json()` then throws a `SyntaxError` which must surface as
    // `server_error`, not a raw exception.
    const fakeFetch = async () =>
      new Response("<html>oops</html>", {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    const client = new CloudClient({
      ...BASE_OPTS,
      fetch: fakeFetch as typeof fetch,
    });
    await expect(
      client.browser({
        deviceId: "d",
        sessionId: "s",
        action: "snapshot",
      }),
    ).rejects.toMatchObject({ code: "server_error" });
  });

  it("bad: AbortError thrown by res.json() body stream → CloudClientError(abort)", async () => {
    // If the caller aborts while the body is still draining, `res.json()`
    // throws an `AbortError`. That must surface as `abort`, not `network`.
    const fakeFetch = async () => {
      const stream = new ReadableStream({
        pull(controller) {
          controller.error(new DOMException("aborted", "AbortError"));
        },
      });
      return new Response(stream, {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    };
    const client = new CloudClient({
      ...BASE_OPTS,
      fetch: fakeFetch as typeof fetch,
    });
    await expect(
      client.browser({
        deviceId: "d",
        sessionId: "s",
        action: "snapshot",
      }),
    ).rejects.toMatchObject({ code: "abort" });
  });

  it("bad: generic error in res.json() body stream → CloudClientError(network)", async () => {
    // A mid-body `TypeError` (e.g. socket reset) must surface as `network`.
    const fakeFetch = async () => {
      const stream = new ReadableStream({
        pull(controller) {
          controller.error(new TypeError("network reset"));
        },
      });
      return new Response(stream, {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    };
    const client = new CloudClient({
      ...BASE_OPTS,
      fetch: fakeFetch as typeof fetch,
    });
    await expect(
      client.browser({
        deviceId: "d",
        sessionId: "s",
        action: "snapshot",
      }),
    ).rejects.toMatchObject({ code: "network" });
  });

  it("forwards AbortSignal to underlying fetch", async () => {
    // Sanity check: the caller-supplied signal must reach `fetch()` so
    // aborts actually cancel the in-flight request.
    let capturedSignal: AbortSignal | undefined | null;
    const fakeFetch = async (
      _input: unknown,
      init?: RequestInit,
    ): Promise<Response> => {
      capturedSignal = init?.signal;
      return new Response(
        JSON.stringify({ success: true, duration_ms: 1 }),
        {
          status: 200,
          headers: { "Content-Type": "application/json" },
        },
      );
    };
    const ctrl = new AbortController();
    const client = new CloudClient({
      ...BASE_OPTS,
      fetch: fakeFetch as typeof fetch,
    });
    await client.browser({
      deviceId: "d",
      sessionId: "s",
      action: "snapshot",
      signal: ctrl.signal,
    });
    expect(capturedSignal).toBe(ctrl.signal);
  });

  it("rejects malformed root: response is the literal value null", async () => {
    // `JSON.parse("null")` is the literal value `null`. Reading
    // `.success` off it would throw `TypeError: Cannot read properties
    // of null` — guard must reject before that.
    const fakeFetch = async () =>
      new Response("null", {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    const client = new CloudClient({
      ...BASE_OPTS,
      fetch: fakeFetch as typeof fetch,
    });
    await expect(
      client.browser({
        deviceId: "d",
        sessionId: "s",
        action: "snapshot",
      }),
    ).rejects.toMatchObject({ code: "server_error" });
  });

  it("rejects malformed root: response is a non-object (number)", async () => {
    // A misbehaving proxy could emit a bare number/string. The root
    // check must reject these as malformed instead of crashing on
    // property access.
    const fakeFetch = async () =>
      new Response("42", {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    const client = new CloudClient({
      ...BASE_OPTS,
      fetch: fakeFetch as typeof fetch,
    });
    await expect(
      client.browser({
        deviceId: "d",
        sessionId: "s",
        action: "snapshot",
      }),
    ).rejects.toMatchObject({ code: "server_error" });
  });

  it("rejects malformed root: response is a JSON array", async () => {
    // `JSON.parse("[]")` returns an array — `typeof [] === "object"` so
    // it slips past the `typeof !== "object"` check, but reading
    // `.success` returns `undefined`, which fails the boolean check.
    // Pin this so a future refactor that loosens the boolean check
    // (e.g. coerces with `Boolean(json.success)`) does not silently
    // start accepting array roots.
    const fakeFetch = async () =>
      new Response("[]", {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    const client = new CloudClient({
      ...BASE_OPTS,
      fetch: fakeFetch as typeof fetch,
    });
    await expect(
      client.browser({
        deviceId: "d",
        sessionId: "s",
        action: "snapshot",
      }),
    ).rejects.toMatchObject({ code: "server_error" });
  });
});

describe("CloudClient.files", () => {
  it("happy: POSTs /api/control/files with snake_case body + Bearer auth", async () => {
    const { fn, calls } = mockFetch([
      () =>
        jsonResponse(
          {
            request_id: "req-001",
            operation: "stat",
            success: true,
            result: {
              path: "/tmp/x",
              file_type: "file",
              size: 42,
              modified_ms: 0,
              created_ms: 0,
              accessed_ms: 0,
            },
            duration_ms: 7,
          },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });

    const r = await client.files({
      deviceId: "d1",
      operation: "stat",
      params: { path: "/tmp/x" },
      timeoutMs: 5_000,
      correlationId: "c-1",
    });

    expect(calls[0].url).toBe("https://hub.test/api/control/files");
    expect(calls[0].init?.method).toBe("POST");
    expect(calls[0].init?.headers).toMatchObject({
      Authorization: "Bearer test-token",
      "Content-Type": "application/json",
    });
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body).toEqual({
      device_id: "d1",
      operation: "stat",
      params: { path: "/tmp/x" },
      timeout_ms: 5_000,
      correlation_id: "c-1",
    });
    expect(r.success).toBe(true);
    expect(r.requestId).toBe("req-001");
    expect(r.operation).toBe("stat");
    // result is `unknown` — cast at the call site as needed.
    expect((r.result as { size: number }).size).toBe(42);
    expect(r.error).toBeUndefined();
    expect(r.durationMs).toBe(7);
  });

  it("happy: omitted params field serializes as empty object {}, no correlation_id", async () => {
    const { fn, calls } = mockFetch([
      () =>
        jsonResponse(
          { request_id: "r", operation: "list", success: true, result: { entries: [] }, duration_ms: 1 },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await client.files({ deviceId: "d", operation: "list" });
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body.params).toEqual({});
    expect("correlation_id" in body).toBe(false);
    // timeout_ms is omitted when caller doesn't set it (the hub applies
    // its own default — we don't enforce one client-side).
    expect("timeout_ms" in body).toBe(false);
  });

  it("happy: surfaces daemon-side error inside the result envelope", async () => {
    // policy_denied is the canonical "daemon refused" case. The SDK
    // surfaces it as `success: false` plus an `error` field — NOT as a
    // thrown CloudClientError.
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          {
            request_id: "r-pol",
            operation: "delete",
            success: false,
            error: {
              code: "policy_denied",
              message: "policy refused: protected path",
              path: "/etc/shadow",
            },
            duration_ms: 4,
          },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const r = await client.files({
      deviceId: "d",
      operation: "delete",
      params: { path: "/etc/shadow", recursive: false, mode: "permanent" },
    });
    expect(r.success).toBe(false);
    expect(r.error?.code).toBe("policy_denied");
    expect(r.error?.path).toBe("/etc/shadow");
    expect(r.error?.message).toContain("policy refused");
    expect(r.result).toBeUndefined();
  });

  it("happy: round-trips a list response with multiple entries", async () => {
    // Lock the multi-entry serializer's snake_case field names — these
    // are part of the public wire contract.
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          {
            request_id: "r-list",
            operation: "list",
            success: true,
            result: {
              entries: [
                { name: "a", file_type: "file", size: 1, modified_ms: 0, symlink_target: null },
                { name: "b", file_type: "directory", size: 0, modified_ms: 0, symlink_target: null },
              ],
              total_count: 2,
              has_more: false,
            },
            duration_ms: 3,
          },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const r = await client.files({ deviceId: "d", operation: "list", params: { path: "/tmp" } });
    expect(r.success).toBe(true);
    const result = r.result as { entries: { name: string; file_type: string }[] };
    expect(result.entries.length).toBe(2);
    expect(result.entries[0].name).toBe("a");
    expect(result.entries[0].file_type).toBe("file");
    expect(result.entries[1].file_type).toBe("directory");
  });

  it("happy: write op forwards full_write.content payload", async () => {
    // Verify the SDK forwards op-specific params verbatim — no
    // client-side path validation, no schema massage.
    const { fn, calls } = mockFetch([
      () =>
        jsonResponse(
          {
            request_id: "r-w",
            operation: "write",
            success: true,
            result: { path: "/tmp/out.txt", action: "created", bytes_written: 11, final_size: 11 },
            duration_ms: 2,
          },
          200,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await client.files({
      deviceId: "d",
      operation: "write",
      params: {
        path: "/tmp/out.txt",
        create_parents: true,
        full_write: { content: "hello world" },
      },
    });
    const body = JSON.parse(calls[0].init?.body as string);
    expect(body.params.full_write.content).toBe("hello world");
    expect(body.params.create_parents).toBe(true);
  });

  it("bad: HTTP 401 → CloudClientError(unauthorized)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ error: { code: "UNAUTHORIZED", message: "no token" } }, 401),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "unauthorized", httpStatus: 401 });
  });

  it("bad: HTTP 409 with DEVICE_OFFLINE → CloudClientError(device_offline)", async () => {
    // The hub returns 409 (not 404) for an offline-but-known device on
    // the files endpoint. The SDK should surface this as `device_offline`
    // — distinct from `not_found` (truly unknown device) and from
    // `bad_request` (other 409s, none today).
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          { error: { code: "DEVICE_OFFLINE", message: "Device d is not currently connected" } },
          409,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "device_offline", httpStatus: 409 });
  });

  it("bad: HTTP 504 → CloudClientError(timeout)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ error: { code: "DEVICE_TIMEOUT", message: "deadline exceeded" } }, 504),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "timeout", httpStatus: 504 });
  });

  it("bad: HTTP 5xx → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ error: { code: "INTERNAL_ERROR", message: "oops" } }, 500),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "server_error", httpStatus: 500 });
  });

  it("bad: HTTP 400 INVALID_PARAMS → CloudClientError(bad_request)", async () => {
    const { fn } = mockFetch([
      () =>
        jsonResponse(
          { error: { code: "INVALID_PARAMS", message: "missing field 'path'" } },
          400,
        ),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: {} }),
    ).rejects.toMatchObject({ code: "bad_request", httpStatus: 400 });
  });

  it("bad: aborted before request → throws abort without fetching", async () => {
    const { fn, calls } = mockFetch([]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    const ctrl = new AbortController();
    ctrl.abort();
    await expect(
      client.files({
        deviceId: "d",
        operation: "stat",
        params: { path: "/tmp/x" },
        signal: ctrl.signal,
      }),
    ).rejects.toMatchObject({ code: "abort" });
    expect(calls.length).toBe(0);
  });

  it("bad: aborted after token fetch → throws abort without fetching", async () => {
    const { fn, calls } = mockFetch([]);
    const ctrl = new AbortController();
    const client = new CloudClient({
      hubUrl: "https://hub.test",
      // Abort during the token fetch — exercises the post-token-fetch
      // re-check.
      getAuthToken: async () => {
        ctrl.abort();
        return "tok";
      },
      fetch: fn,
    });
    await expect(
      client.files({
        deviceId: "d",
        operation: "stat",
        params: { path: "/tmp/x" },
        signal: ctrl.signal,
      }),
    ).rejects.toMatchObject({ code: "abort" });
    expect(calls.length).toBe(0);
  });

  it("bad: fetch network failure → CloudClientError(network)", async () => {
    const fn = vi.fn(async () => {
      throw new TypeError("connect ECONNREFUSED");
    }) as unknown as typeof fetch;
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "network" });
  });

  it("bad: fetch AbortError → CloudClientError(abort)", async () => {
    const fn = vi.fn(async () => {
      const e = new Error("aborted");
      e.name = "AbortError";
      throw e;
    }) as unknown as typeof fetch;
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "abort" });
  });

  it("bad: malformed JSON body → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([
      () =>
        new Response("<html>not json</html>", {
          status: 200,
          headers: { "Content-Type": "text/html" },
        }),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "server_error" });
  });

  it("bad: response missing 'success' boolean → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([
      () => jsonResponse({ request_id: "r", operation: "stat", duration_ms: 0 }, 200),
    ]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "server_error" });
  });

  it("bad: response root is an array → CloudClientError(server_error)", async () => {
    const { fn } = mockFetch([() => jsonResponse([], 200)]);
    const client = new CloudClient({ ...BASE_OPTS, fetch: fn });
    await expect(
      client.files({ deviceId: "d", operation: "stat", params: { path: "/tmp/x" } }),
    ).rejects.toMatchObject({ code: "server_error" });
  });
});

describe("CloudClientError", () => {
  it("exposes expected properties", () => {
    const err = new CloudClientError("forbidden", "msg", {
      httpStatus: 403,
      jobErrorCode: "denied",
      jobErrorMessage: "access denied",
      cause: new Error("inner"),
    });
    expect(err.name).toBe("CloudClientError");
    expect(err.code).toBe("forbidden");
    expect(err.httpStatus).toBe(403);
    expect(err.jobErrorCode).toBe("denied");
    expect(err.jobErrorMessage).toBe("access denied");
    expect(err.cause).toBeInstanceOf(Error);
    expect(err).toBeInstanceOf(Error);
  });
});
