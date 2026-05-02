# Dashboard Files Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers-extended-cc:subagent-driven-development (recommended) or superpowers-extended-cc:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "Files" tab to the hub-dashboard device detail page so operators can browse, view, create, delete, download, and upload files on a connected device via the existing `POST /api/devices/{id}/files` protobuf endpoint.

**Architecture:** A new client component `DeviceFiles` drives the flow. It calls `POST /api/proxy/api/devices/{id}/files` with a protobuf-encoded `FileRequest` body (`application/x-protobuf`) and decodes the `FileResponse`. A thin `file-ops-client.ts` helper handles encode/decode, fetch, and the 4xx/5xx JSON-error-envelope path. The dashboard proxy route gains a POST handler that passes body bytes + `content-type` through to the hub. The backend is already v1 — no hub changes.

**Tech Stack:** Next.js 16 (App Router, React 19), TypeScript, `@ahandai/proto` (ts-proto generated), vitest + @testing-library/react + jsdom. No new UI libraries — reuse existing `.surface-panel` / `.browser-*` / Tailwind utility patterns from `globals.css`.

**Spec:** Inline in PR description (no separate spec file — this is a UI-only feature aligned with already-shipped backend).

---

## Scope & Constraints

**In scope (v1):**
- `list`, `mkdir`, `delete` (with `recursive` toggle)
- `read_text` for text files, `read_image` for images, binary placeholder
- `download` (via `read_binary` → Blob)
- `upload` via `FullWrite.content` (inline) with **hard 1 MiB ceiling** — anything larger shows "S3 path not implemented yet" (PR #22 tracks the S3 e2e flow per `docs/superpowers/specs/2026-04-12-device-file-operations-design.md`)

**Out of scope:**
- `edit` / `chmod` / `copy` / `move` / `create_symlink` — no UI
- `stat` / `glob` — no dedicated UI (list is sufficient for v1)
- Permission / approval UI — server-side STRICT-mode handles approvals; 4xx error envelope is enough
- Large-file S3 transfer — blocked on PR #22
- Client-side path validation — backend `file_policy` is authoritative, we only display its errors

---

## File Structure

### New files
| File | Responsibility |
|------|---------------|
| `apps/hub-dashboard/src/lib/file-ops-client.ts` | Typed request builders + encode/decode + fetch + error-envelope handling |
| `apps/hub-dashboard/src/components/device-files.tsx` | Files panel UI: breadcrumb, entry list, action bar, viewer, dialogs |
| `apps/hub-dashboard/tests/device-files.test.tsx` | Component tests (list/mkdir/delete/upload/error envelope) |
| `apps/hub-dashboard/tests/file-ops-client.test.ts` | Helper tests (encode roundtrip + error envelope parsing + >1MB guard) |

### Modified files
| File | Change |
|------|--------|
| `apps/hub-dashboard/package.json` | Add `@ahandai/proto: workspace:*` + `buffer: ^6.0.3` runtime deps |
| `packages/proto-ts/src/index.ts` | Re-export `FileRequest`, `FileResponse`, `FileError`, `FileEntry`, `FileType`, `FileErrorCode`, `FileListResult`, and related types from `./generated/ahand/v1/file_ops.ts` |
| `apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts` | Add POST handler that forwards body bytes + `content-type` / `accept` headers (pass-through for protobuf) |
| `apps/hub-dashboard/src/components/device-tabs.tsx` | Add "Files" tab (visible when `online === true`) |
| `apps/hub-dashboard/src/app/globals.css` | Add `.files-*` styles, reusing `.surface-panel` / `.device-tabs-*` tokens |
| `apps/hub-dashboard/tests/auth-server.test.ts` | Add a POST-proxy test case (protobuf body passthrough + session/baseurl branches) |

---

## Known Gotchas

1. **`Buffer` in browser bundle.** `packages/proto-ts/src/generated/ahand/v1/file_ops.ts` calls `Buffer.from(...)` and `Buffer.alloc(0)` directly. Next.js 16 does NOT polyfill Node `Buffer` for client bundles. We add the `buffer` npm package and polyfill `globalThis.Buffer` once at module load inside `file-ops-client.ts`. This is the least-invasive fix.
2. **`Uint8Array` vs `Buffer` at call sites.** ts-proto writes use `writer.bytes(...)`; passing a `Uint8Array` works at runtime but the types insist on `Buffer`. We pass through the polyfilled `Buffer.from(uint8)` to stay type-safe.
3. **Proxy content-type preservation.** The existing GET proxy strips body. POST must preserve `content-type: application/x-protobuf` and forward body as a `ReadableStream` / `ArrayBuffer` (not `request.text()`, which corrupts binary).
4. **`FileResponse.error` vs HTTP error envelope.** Two error paths:
   - Hub returns HTTP 4xx/5xx → JSON body `{error: {code, message}}` (per `crates/ahand-hub/src/http/api_error.rs`).
   - Hub returns HTTP 200 with a `FileResponse` whose `error` oneof is set (policy denied, path not found, etc.).
   Both paths must surface the message in the UI.
5. **`FileEntry.fileType` is a proto enum.** Values are `FILE_TYPE_REGULAR / DIRECTORY / SYMLINK / OTHER`. UI uses numeric comparisons via the exported `FileType` enum — do NOT stringify server-side.
6. **`modifiedMs` is a number (long).** ts-proto emits it as `number` (loses precision past 2^53 ms ≈ year 287396). Safe to pass to `new Date(ms)`.
7. **`@ahandai/proto` package is not currently a dep of hub-dashboard.** This is the first app-level consumer of proto-ts in the dashboard — confirm `tsc` sees the types before hitting runtime.

---

### Task 0: Workspace wiring — add proto dep + re-export file_ops types + Buffer polyfill

**Goal:** Make `FileRequest` / `FileResponse` and related types importable from `@ahandai/proto` inside `apps/hub-dashboard`, and ensure `Buffer` works in the browser bundle.

**Files:**
- Modify: `apps/hub-dashboard/package.json`
- Modify: `packages/proto-ts/src/index.ts`

**Acceptance Criteria:**
- [ ] `import { FileRequest, FileResponse, FileEntry, FileType, FileErrorCode } from "@ahandai/proto";` type-checks inside `apps/hub-dashboard/src/**`
- [ ] `pnpm install` succeeds from repo root
- [ ] `pnpm --filter @ahandai/proto build` succeeds
- [ ] `pnpm --filter @ahand/hub-dashboard exec tsc --noEmit` succeeds

**Verify:** `pnpm --filter @ahand/hub-dashboard exec tsc --noEmit` → zero errors

**Steps:**

- [ ] **Step 1: Add `@ahandai/proto` and `buffer` to hub-dashboard deps**

Edit `apps/hub-dashboard/package.json`. In `"dependencies"`, add (alphabetically):

```json
"@ahandai/proto": "workspace:*",
"buffer": "^6.0.3",
```

- [ ] **Step 2: Re-export file_ops types from `@ahandai/proto`**

Edit `packages/proto-ts/src/index.ts`. Append to the file:

```ts
export {
  FileRequest,
  FileResponse,
  FileError,
  FileEntry,
  FileType,
  FileErrorCode,
  FileReadText,
  FileReadTextResult,
  FileReadBinary,
  FileReadBinaryResult,
  FileReadImage,
  FileReadImageResult,
  FileWrite,
  FullWrite,
  FileDelete,
  FileDeleteResult,
  FileList,
  FileListResult,
  FileMkdir,
  FileMkdirResult,
  ImageFormat,
  DeleteMode,
  WriteAction,
  fileErrorCodeToJSON,
  fileTypeToJSON,
} from "./generated/ahand/v1/file_ops.ts";

export type {
  FileRequest as FileRequestMsg,
  FileResponse as FileResponseMsg,
} from "./generated/ahand/v1/file_ops.ts";
```

- [ ] **Step 3: Install and build proto package**

Run from repo root:
```bash
pnpm install
pnpm --filter @ahandai/proto build
```
Expected: clean install, `packages/proto-ts/dist/index.js` and `index.d.ts` present, no type errors.

- [ ] **Step 4: Verify dashboard sees the new types**

Create a scratch file `apps/hub-dashboard/src/lib/_proto_smoke.ts` (temporary):

```ts
import { FileRequest, FileType, FileErrorCode } from "@ahandai/proto";
export const _smoke = { FileRequest, FileType, FileErrorCode };
```

Run: `pnpm --filter @ahand/hub-dashboard exec tsc --noEmit`
Expected: zero errors.

Delete the scratch file:
```bash
rm apps/hub-dashboard/src/lib/_proto_smoke.ts
```

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/package.json packages/proto-ts/src/index.ts pnpm-lock.yaml
git commit -m "feat(dashboard): wire @ahandai/proto + buffer deps for file ops"
```

---

### Task 1: Dashboard proxy POST handler for protobuf

**Goal:** Teach the dashboard proxy route to forward POST requests (protobuf or JSON) through to the hub with body + `content-type` preserved.

**Files:**
- Modify: `apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts`
- Modify: `apps/hub-dashboard/tests/auth-server.test.ts`

**Acceptance Criteria:**
- [ ] `POST /api/proxy/<path>` forwards body as raw `ArrayBuffer` (no text coercion)
- [ ] `content-type` header is forwarded verbatim; `accept` is forwarded (default `application/octet-stream` for protobuf responses)
- [ ] `authorization: Bearer <session>` is added
- [ ] Missing cookie → 401 JSON `{error: {code: "unauthorized", message}}`
- [ ] Missing `AHAND_HUB_BASE_URL` → 503 JSON `{error: {code: "hub_unavailable", message}}`
- [ ] Upstream `fetch` error → 503 `{error: {code: "hub_unavailable", message}}`
- [ ] Response body, status, and `content-type` header are proxied back

**Verify:** `pnpm --filter @ahand/hub-dashboard test -- auth-server` → all tests pass

**Steps:**

- [ ] **Step 1: Write the failing test**

Append to `apps/hub-dashboard/tests/auth-server.test.ts` inside the existing `describe("GET /api/proxy/*", ...)` sibling scope — add a new `describe`:

```ts
describe("POST /api/proxy/*", () => {
  const HUB_BASE_URL = "http://hub.internal:8080";

  beforeEach(() => {
    vi.stubEnv("AHAND_HUB_BASE_URL", HUB_BASE_URL);
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
  });

  it("forwards protobuf bodies with content-type preserved", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(new Uint8Array([0x0a, 0x03, 0x66, 0x6f, 0x6f]), {
        status: 200,
        headers: { "content-type": "application/x-protobuf" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { POST: proxyPost } = await import("@/app/api/proxy/[...path]/route");
    const bodyBytes = new Uint8Array([0x08, 0x01, 0x12, 0x04, 0x74, 0x65, 0x73, 0x74]);
    const request = new NextRequest(
      "http://localhost/api/proxy/api/devices/dev-1/files",
      {
        method: "POST",
        headers: {
          "content-type": "application/x-protobuf",
          accept: "application/x-protobuf",
          cookie: "ahand_hub_session=session-token",
        },
        body: bodyBytes,
      },
    );

    const response = await proxyPost(request, {
      params: Promise.resolve({ path: ["api", "devices", "dev-1", "files"] }),
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [calledUrl, calledInit] = fetchMock.mock.calls[0];
    expect(calledUrl).toBe(`${HUB_BASE_URL}/api/devices/dev-1/files`);
    expect(calledInit.method).toBe("POST");
    expect(calledInit.headers).toMatchObject({
      authorization: "Bearer session-token",
      "content-type": "application/x-protobuf",
      accept: "application/x-protobuf",
    });
    // Body must be forwarded verbatim as bytes.
    const forwarded = new Uint8Array(
      calledInit.body instanceof ArrayBuffer
        ? calledInit.body
        : (calledInit.body as Uint8Array).buffer,
    );
    expect(Array.from(forwarded)).toEqual(Array.from(bodyBytes));
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("application/x-protobuf");
  });

  it("returns 401 JSON envelope when session cookie is missing", async () => {
    const { POST: proxyPost } = await import("@/app/api/proxy/[...path]/route");
    const request = new NextRequest("http://localhost/api/proxy/api/devices/x/files", {
      method: "POST",
      headers: { "content-type": "application/x-protobuf" },
      body: new Uint8Array([0x00]),
    });
    const response = await proxyPost(request, {
      params: Promise.resolve({ path: ["api", "devices", "x", "files"] }),
    });
    expect(response.status).toBe(401);
    const body = await response.json();
    expect(body.error.code).toBe("unauthorized");
  });

  it("returns 503 when AHAND_HUB_BASE_URL is missing", async () => {
    vi.unstubAllEnvs();
    const { POST: proxyPost } = await import("@/app/api/proxy/[...path]/route");
    const request = new NextRequest("http://localhost/api/proxy/api/devices/x/files", {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
        cookie: "ahand_hub_session=session-token",
      },
      body: new Uint8Array([0x00]),
    });
    const response = await proxyPost(request, {
      params: Promise.resolve({ path: ["api", "devices", "x", "files"] }),
    });
    expect(response.status).toBe(503);
    const body = await response.json();
    expect(body.error.code).toBe("hub_unavailable");
  });
});
```

- [ ] **Step 2: Run the failing test**

```bash
pnpm --filter @ahand/hub-dashboard test -- auth-server
```
Expected: FAIL — `POST` is not exported from the route.

- [ ] **Step 3: Add the POST handler**

Edit `apps/hub-dashboard/src/app/api/proxy/[...path]/route.ts`. Append after the existing `GET` function:

```ts
export async function POST(
  request: NextRequest,
  { params }: { params: Promise<{ path: string[] }> },
) {
  const { path } = await params;
  const session = request.cookies.get("ahand_hub_session")?.value ?? "";
  const baseUrl = process.env.AHAND_HUB_BASE_URL;

  if (!session) {
    return dashboardErrorResponse("unauthorized", "Sign in required.", 401);
  }

  if (!baseUrl) {
    return dashboardErrorResponse("hub_unavailable", "Unable to reach the hub right now.", 503);
  }

  const upstream = new URL(path.join("/"), `${baseUrl.replace(/\/?$/, "/")}`);
  upstream.search = request.nextUrl.search;

  let response: Response;
  try {
    const bodyBuffer = await request.arrayBuffer();
    const headers: Record<string, string> = {
      authorization: `Bearer ${session}`,
      "content-type": request.headers.get("content-type") ?? "application/octet-stream",
      accept: request.headers.get("accept") ?? "application/octet-stream",
    };
    response = await fetch(upstream.toString(), {
      method: "POST",
      headers,
      body: bodyBuffer,
      cache: "no-store",
    });
  } catch {
    return dashboardErrorResponse("hub_unavailable", "Unable to reach the hub right now.", 503);
  }

  return new NextResponse(response.body, {
    status: response.status,
    headers: response.headers,
  });
}
```

- [ ] **Step 4: Rerun the test**

```bash
pnpm --filter @ahand/hub-dashboard test -- auth-server
```
Expected: all POST cases pass, existing GET cases still pass.

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/src/app/api/proxy apps/hub-dashboard/tests/auth-server.test.ts
git commit -m "feat(dashboard): add pass-through POST handler for /api/proxy"
```

---

### Task 2: `file-ops-client.ts` — typed request builders + encode/decode + error handling

**Goal:** Provide one small, well-tested module the React component can call. It hides protobuf serialization, the Buffer polyfill, and the two error-envelope paths.

**Files:**
- Create: `apps/hub-dashboard/src/lib/file-ops-client.ts`
- Create: `apps/hub-dashboard/tests/file-ops-client.test.ts`

**Acceptance Criteria:**
- [ ] Exports named async functions: `listFiles`, `readText`, `readImage`, `readBinary`, `mkdir`, `deleteFile`, `writeFile`
- [ ] Each function accepts a `deviceId` + operation-specific args and returns the parsed `FileResponse` oneof payload (or throws a typed error)
- [ ] Exports `FileOpsError extends Error` with `code: string`, `message: string`, and optional `httpStatus: number`
- [ ] Maps HTTP 4xx/5xx with JSON `{error: {code, message}}` → `FileOpsError(code, message, httpStatus)`
- [ ] Maps `FileResponse.error` (proto-level) → `FileOpsError(fileErrorCodeToJSON(code), message, 200)`
- [ ] `writeFile` rejects with `FileOpsError("content_too_large", "S3 path not implemented yet", 0)` when `content.byteLength > 1_048_576`
- [ ] Polyfills `globalThis.Buffer` at module-load via `import { Buffer } from "buffer"` (idempotent)

**Verify:** `pnpm --filter @ahand/hub-dashboard test -- file-ops-client` → all tests pass

**Steps:**

- [ ] **Step 1: Write failing tests**

Create `apps/hub-dashboard/tests/file-ops-client.test.ts`:

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { FileRequest, FileResponse, FileType } from "@ahandai/proto";

import {
  FileOpsError,
  listFiles,
  mkdir,
  readText,
  writeFile,
} from "@/lib/file-ops-client";

describe("file-ops-client", () => {
  const deviceId = "dev-1";
  const expectedUrl = "/api/proxy/api/devices/dev-1/files";

  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn());
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function respondWith(resp: FileResponse) {
    const bytes = FileResponse.encode(resp).finish();
    (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(bytes, {
        status: 200,
        headers: { "content-type": "application/x-protobuf" },
      }),
    );
  }

  it("listFiles encodes FileList and decodes FileListResult", async () => {
    respondWith(
      FileResponse.fromPartial({
        requestId: "req-1",
        list: {
          entries: [
            {
              name: "hello.txt",
              fileType: FileType.FILE_TYPE_REGULAR,
              size: 42,
              modifiedMs: 1_700_000_000_000,
            },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );

    const result = await listFiles(deviceId, { path: "/tmp" });
    expect(result.entries).toHaveLength(1);
    expect(result.entries[0].name).toBe("hello.txt");

    const mock = globalThis.fetch as ReturnType<typeof vi.fn>;
    expect(mock).toHaveBeenCalledWith(
      expectedUrl,
      expect.objectContaining({
        method: "POST",
        headers: expect.objectContaining({
          "content-type": "application/x-protobuf",
          accept: "application/x-protobuf",
        }),
      }),
    );
    const body = mock.mock.calls[0][1].body as Uint8Array;
    const decoded = FileRequest.decode(body);
    expect(decoded.list?.path).toBe("/tmp");
  });

  it("maps HTTP 4xx JSON envelope to FileOpsError", async () => {
    (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          error: { code: "POLICY_DENIED", message: "/etc/passwd is in dangerous_paths" },
        }),
        { status: 403, headers: { "content-type": "application/json" } },
      ),
    );
    await expect(listFiles(deviceId, { path: "/etc" })).rejects.toMatchObject({
      code: "POLICY_DENIED",
      message: expect.stringContaining("dangerous_paths"),
      httpStatus: 403,
    });
  });

  it("maps FileResponse.error (200 OK with proto error) to FileOpsError", async () => {
    respondWith(
      FileResponse.fromPartial({
        requestId: "req-1",
        error: {
          code: 12, // FILE_ERROR_CODE_POLICY_DENIED
          message: "blocked by policy",
          path: "/etc/shadow",
        },
      }),
    );
    await expect(readText(deviceId, { path: "/etc/shadow" })).rejects.toMatchObject({
      code: "FILE_ERROR_CODE_POLICY_DENIED",
      message: expect.stringContaining("blocked"),
      httpStatus: 200,
    });
  });

  it("mkdir succeeds and decodes result", async () => {
    respondWith(
      FileResponse.fromPartial({
        requestId: "req-1",
        mkdir: { path: "/tmp/new", alreadyExisted: false },
      }),
    );
    const r = await mkdir(deviceId, { path: "/tmp/new", recursive: true });
    expect(r.path).toBe("/tmp/new");
    expect(r.alreadyExisted).toBe(false);
  });

  it("writeFile rejects payloads > 1 MiB with content_too_large", async () => {
    const big = new Uint8Array(1_048_577);
    await expect(
      writeFile(deviceId, { path: "/tmp/big.bin", content: big }),
    ).rejects.toMatchObject({
      code: "content_too_large",
      message: expect.stringContaining("S3"),
    });
    expect(globalThis.fetch).not.toHaveBeenCalled();
  });

  it("FileOpsError is an Error subclass", () => {
    const e = new FileOpsError("X", "y", 400);
    expect(e).toBeInstanceOf(Error);
    expect(e.code).toBe("X");
    expect(e.httpStatus).toBe(400);
  });
});
```

- [ ] **Step 2: Run tests, confirm they fail**

```bash
pnpm --filter @ahand/hub-dashboard test -- file-ops-client
```
Expected: FAIL — module does not exist.

- [ ] **Step 3: Implement `file-ops-client.ts`**

Create `apps/hub-dashboard/src/lib/file-ops-client.ts`:

```ts
import { Buffer } from "buffer";
import {
  FileRequest,
  FileResponse,
  FileListResult,
  FileMkdirResult,
  FileDeleteResult,
  FileReadTextResult,
  FileReadImageResult,
  FileReadBinaryResult,
  FileWriteResult,
  fileErrorCodeToJSON,
} from "@ahandai/proto";

if (typeof globalThis.Buffer === "undefined") {
  (globalThis as { Buffer: typeof Buffer }).Buffer = Buffer;
}

const MAX_INLINE_WRITE_BYTES = 1_048_576;

export class FileOpsError extends Error {
  readonly code: string;
  readonly httpStatus: number;
  constructor(code: string, message: string, httpStatus: number) {
    super(message);
    this.name = "FileOpsError";
    this.code = code;
    this.httpStatus = httpStatus;
  }
}

function proxyUrl(deviceId: string): string {
  return `/api/proxy/api/devices/${encodeURIComponent(deviceId)}/files`;
}

function newRequestId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `req-${Math.random().toString(36).slice(2)}-${Date.now().toString(36)}`;
}

async function sendRequest(deviceId: string, req: FileRequest): Promise<FileResponse> {
  const body = FileRequest.encode(req).finish();
  const res = await fetch(proxyUrl(deviceId), {
    method: "POST",
    headers: {
      "content-type": "application/x-protobuf",
      accept: "application/x-protobuf",
    },
    body,
    cache: "no-store",
  });
  if (!res.ok) {
    let code = `http_${res.status}`;
    let message = `Hub returned HTTP ${res.status}`;
    try {
      const envelope = (await res.clone().json()) as {
        error?: { code?: string; message?: string };
      };
      if (envelope?.error) {
        code = envelope.error.code ?? code;
        message = envelope.error.message ?? message;
      }
    } catch {
      try {
        message = (await res.text()) || message;
      } catch {
        // swallow — keep default
      }
    }
    throw new FileOpsError(code, message, res.status);
  }
  const raw = new Uint8Array(await res.arrayBuffer());
  const resp = FileResponse.decode(raw);
  if (resp.error) {
    throw new FileOpsError(
      fileErrorCodeToJSON(resp.error.code),
      resp.error.message || "file operation failed",
      200,
    );
  }
  return resp;
}

export interface ListFilesArgs {
  path: string;
  includeHidden?: boolean;
  maxResults?: number;
  offset?: number;
}

export async function listFiles(deviceId: string, args: ListFilesArgs): Promise<FileListResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    list: {
      path: args.path,
      includeHidden: args.includeHidden ?? false,
      maxResults: args.maxResults,
      offset: args.offset,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.list) {
    throw new FileOpsError("malformed_response", "list response missing", 200);
  }
  return resp.list;
}

export interface ReadTextArgs {
  path: string;
  maxLines?: number;
  maxBytes?: number;
}

export async function readText(
  deviceId: string,
  args: ReadTextArgs,
): Promise<FileReadTextResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    readText: {
      path: args.path,
      maxLines: args.maxLines ?? 2000,
      maxBytes: args.maxBytes ?? 65_536,
      lineNumbers: false,
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.readText) {
    throw new FileOpsError("malformed_response", "read_text response missing", 200);
  }
  return resp.readText;
}

export interface ReadImageArgs {
  path: string;
  maxBytes?: number;
}

export async function readImage(
  deviceId: string,
  args: ReadImageArgs,
): Promise<FileReadImageResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    readImage: {
      path: args.path,
      maxBytes: args.maxBytes ?? 1_048_576,
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.readImage) {
    throw new FileOpsError("malformed_response", "read_image response missing", 200);
  }
  return resp.readImage;
}

export interface ReadBinaryArgs {
  path: string;
  maxBytes?: number;
}

export async function readBinary(
  deviceId: string,
  args: ReadBinaryArgs,
): Promise<FileReadBinaryResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    readBinary: {
      path: args.path,
      byteOffset: 0,
      byteLength: 0,
      maxBytes: args.maxBytes ?? MAX_INLINE_WRITE_BYTES,
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.readBinary) {
    throw new FileOpsError("malformed_response", "read_binary response missing", 200);
  }
  return resp.readBinary;
}

export interface MkdirArgs {
  path: string;
  recursive?: boolean;
}

export async function mkdir(deviceId: string, args: MkdirArgs): Promise<FileMkdirResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    mkdir: { path: args.path, recursive: args.recursive ?? true },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.mkdir) {
    throw new FileOpsError("malformed_response", "mkdir response missing", 200);
  }
  return resp.mkdir;
}

export interface DeleteArgs {
  path: string;
  recursive?: boolean;
}

export async function deleteFile(
  deviceId: string,
  args: DeleteArgs,
): Promise<FileDeleteResult> {
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    delete: {
      path: args.path,
      recursive: args.recursive ?? false,
      mode: 0, // DELETE_MODE_UNSPECIFIED — daemon picks safe default
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.delete) {
    throw new FileOpsError("malformed_response", "delete response missing", 200);
  }
  return resp.delete;
}

export interface WriteArgs {
  path: string;
  content: Uint8Array;
  createParents?: boolean;
}

export async function writeFile(
  deviceId: string,
  args: WriteArgs,
): Promise<FileWriteResult> {
  if (args.content.byteLength > MAX_INLINE_WRITE_BYTES) {
    throw new FileOpsError(
      "content_too_large",
      `Uploads larger than ${MAX_INLINE_WRITE_BYTES} bytes require the S3 path, which is not implemented yet.`,
      0,
    );
  }
  const req = FileRequest.fromPartial({
    requestId: newRequestId(),
    write: {
      path: args.path,
      createParents: args.createParents ?? true,
      fullWrite: { content: Buffer.from(args.content) },
      noFollowSymlink: false,
    },
  });
  const resp = await sendRequest(deviceId, req);
  if (!resp.write) {
    throw new FileOpsError("malformed_response", "write response missing", 200);
  }
  return resp.write;
}
```

- [ ] **Step 4: Run tests, expect pass**

```bash
pnpm --filter @ahand/hub-dashboard test -- file-ops-client
```
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/src/lib/file-ops-client.ts apps/hub-dashboard/tests/file-ops-client.test.ts
git commit -m "feat(dashboard): add file-ops-client (encode/decode + error envelope)"
```

---

### Task 3: `DeviceFiles` component — list, navigate, view (text/image/binary)

**Goal:** Render a Files panel that lets the operator type a path, list a directory, click into subfolders via breadcrumbs, and preview text/image files. Error envelope messages display inline.

**Files:**
- Create: `apps/hub-dashboard/src/components/device-files.tsx`
- Create: `apps/hub-dashboard/tests/device-files.test.tsx`

**Acceptance Criteria:**
- [ ] Path input + "Open" button → calls `listFiles`, renders entries sorted dirs-first then alphabetically
- [ ] Breadcrumb for path segments, clicking a segment re-lists that parent
- [ ] Each entry shows name, file-type badge (DIR / FILE / LNK), size (human-readable for files), and modified time
- [ ] Clicking a directory navigates into it; clicking a regular file opens a viewer (text or image or "binary — use Download")
- [ ] Viewer supports Close; error envelope code+message renders inline with `role="alert"`
- [ ] Component is keyboard-accessible: path input has label, list items are `<button>`s, viewer Close button is focusable; all interactive elements have `aria-label` where text is ambiguous

**Verify:** `pnpm --filter @ahand/hub-dashboard test -- device-files` → tests listed below pass

**Steps:**

- [ ] **Step 1: Write failing tests (list happy + 4xx error envelope)**

Create `apps/hub-dashboard/tests/device-files.test.tsx`:

```tsx
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FileResponse, FileType } from "@ahandai/proto";

import { DeviceFiles } from "@/components/device-files";

function stubProto(resp: FileResponse) {
  const bytes = FileResponse.encode(resp).finish();
  (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
    new Response(bytes, {
      status: 200,
      headers: { "content-type": "application/x-protobuf" },
    }),
  );
}

function stubErrorEnvelope(code: string, message: string, status = 403) {
  (globalThis.fetch as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
    new Response(JSON.stringify({ error: { code, message } }), {
      status,
      headers: { "content-type": "application/json" },
    }),
  );
}

describe("DeviceFiles", () => {
  beforeEach(() => {
    vi.stubGlobal("fetch", vi.fn());
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it("lists directory entries and shows dirs before files", async () => {
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "readme.txt", fileType: FileType.FILE_TYPE_REGULAR, size: 12, modifiedMs: 0 },
            { name: "src", fileType: FileType.FILE_TYPE_DIRECTORY, size: 0, modifiedMs: 0 },
          ],
          totalCount: 2,
          hasMore: false,
        },
      }),
    );

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);

    await user.clear(screen.getByLabelText(/path/i));
    await user.type(screen.getByLabelText(/path/i), "/home/user");
    await user.click(screen.getByRole("button", { name: /open/i }));

    const entries = await screen.findAllByRole("listitem");
    // src (dir) must come first.
    expect(entries[0]).toHaveTextContent("src");
    expect(entries[1]).toHaveTextContent("readme.txt");
  });

  it("displays 4xx error envelope from hub", async () => {
    stubErrorEnvelope("POLICY_DENIED", "/etc/passwd is in dangerous_paths", 403);

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);

    await user.clear(screen.getByLabelText(/path/i));
    await user.type(screen.getByLabelText(/path/i), "/etc");
    await user.click(screen.getByRole("button", { name: /open/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/POLICY_DENIED/);
    expect(alert).toHaveTextContent(/dangerous_paths/);
  });
});
```

Add `@testing-library/user-event` to devDependencies if not present. Check first:

```bash
grep "@testing-library/user-event" apps/hub-dashboard/package.json
```

If missing, add it:

```bash
pnpm --filter @ahand/hub-dashboard add -D @testing-library/user-event@^14
```

- [ ] **Step 2: Run tests, confirm they fail**

```bash
pnpm --filter @ahand/hub-dashboard test -- device-files
```
Expected: FAIL — component does not exist.

- [ ] **Step 3: Implement `DeviceFiles` (list + navigate + view)**

Create `apps/hub-dashboard/src/components/device-files.tsx`:

```tsx
"use client";

import { useCallback, useMemo, useState } from "react";
import { FileType, type FileEntry } from "@ahandai/proto";
import {
  FileOpsError,
  listFiles,
  readImage,
  readText,
} from "@/lib/file-ops-client";

interface ViewerState {
  kind: "text" | "image" | "binary";
  path: string;
  text?: string;
  imageSrc?: string;
  imageMime?: string;
}

interface ErrorState {
  code: string;
  message: string;
}

export function DeviceFiles({ deviceId }: { deviceId: string }) {
  const [path, setPath] = useState("/tmp");
  const [entries, setEntries] = useState<FileEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<ErrorState | null>(null);
  const [viewer, setViewer] = useState<ViewerState | null>(null);

  const openDirectory = useCallback(
    async (target: string) => {
      setLoading(true);
      setError(null);
      setViewer(null);
      try {
        const result = await listFiles(deviceId, { path: target });
        const sorted = [...result.entries].sort((a, b) => {
          const aDir = a.fileType === FileType.FILE_TYPE_DIRECTORY ? 0 : 1;
          const bDir = b.fileType === FileType.FILE_TYPE_DIRECTORY ? 0 : 1;
          if (aDir !== bDir) return aDir - bDir;
          return a.name.localeCompare(b.name);
        });
        setEntries(sorted);
        setPath(target);
      } catch (e) {
        setEntries(null);
        setError(toErrorState(e));
      } finally {
        setLoading(false);
      }
    },
    [deviceId],
  );

  const openFile = useCallback(
    async (entryPath: string, entryName: string) => {
      setError(null);
      setViewer({ kind: "text", path: entryPath, text: "Loading..." });
      // Heuristic: image extensions → read_image, everything else → try read_text.
      if (isImage(entryName)) {
        try {
          const r = await readImage(deviceId, { path: entryPath });
          const mime = imageMimeFor(r.format);
          const src = `data:${mime};base64,${uint8ToBase64(r.content)}`;
          setViewer({ kind: "image", path: entryPath, imageSrc: src, imageMime: mime });
        } catch (e) {
          if (e instanceof FileOpsError && e.code === "FILE_ERROR_CODE_ENCODING") {
            setViewer({ kind: "binary", path: entryPath });
            return;
          }
          setViewer(null);
          setError(toErrorState(e));
        }
        return;
      }
      try {
        const r = await readText(deviceId, { path: entryPath });
        const joined = r.lines.map((l) => l.content).join("\n");
        setViewer({ kind: "text", path: entryPath, text: joined });
      } catch (e) {
        // Heuristic: encoding failure → binary placeholder.
        if (e instanceof FileOpsError && e.code === "FILE_ERROR_CODE_ENCODING") {
          setViewer({ kind: "binary", path: entryPath });
          return;
        }
        setViewer(null);
        setError(toErrorState(e));
      }
    },
    [deviceId],
  );

  const crumbs = useMemo(() => buildBreadcrumbs(path), [path]);

  return (
    <div className="files-panel">
      <div className="files-section">
        <div className="files-form-row">
          <label className="files-label" htmlFor="files-path-input">
            Path
          </label>
          <input
            id="files-path-input"
            className="files-input"
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/home/user"
            onKeyDown={(e) => e.key === "Enter" && openDirectory(path)}
          />
          <button
            type="button"
            className="files-btn files-btn-primary"
            onClick={() => openDirectory(path)}
            disabled={loading}
          >
            {loading ? "Loading..." : "Open"}
          </button>
        </div>
        <nav aria-label="Path breadcrumbs" className="files-breadcrumbs">
          {crumbs.map((c, i) => (
            <button
              key={`${c.path}-${i}`}
              type="button"
              className="files-breadcrumb"
              onClick={() => openDirectory(c.path)}
            >
              {c.label}
            </button>
          ))}
        </nav>
      </div>

      {error && (
        <div className="files-error" role="alert">
          <strong>{error.code}</strong>: {error.message}
        </div>
      )}

      {entries && (
        <ul className="files-list" aria-label="Directory entries">
          {entries.length === 0 && (
            <li className="files-empty">(empty directory)</li>
          )}
          {entries.map((e) => (
            <li key={e.name} className="files-entry">
              <button
                type="button"
                className="files-entry-btn"
                onClick={() =>
                  e.fileType === FileType.FILE_TYPE_DIRECTORY
                    ? openDirectory(joinPath(path, e.name))
                    : openFile(joinPath(path, e.name), e.name)
                }
                aria-label={`${typeLabel(e.fileType)} ${e.name}`}
              >
                <span className={`files-badge files-badge-${typeLabel(e.fileType).toLowerCase()}`}>
                  {typeLabel(e.fileType)}
                </span>
                <span className="files-entry-name">{e.name}</span>
                <span className="files-entry-size">
                  {e.fileType === FileType.FILE_TYPE_REGULAR ? humanSize(e.size) : ""}
                </span>
                <span className="files-entry-time">{formatMtime(e.modifiedMs)}</span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {viewer && (
        <div className="files-viewer" role="region" aria-label={`Viewer for ${viewer.path}`}>
          <div className="files-viewer-header">
            <span className="files-viewer-path">{viewer.path}</span>
            <button
              type="button"
              className="files-btn files-btn-sm"
              onClick={() => setViewer(null)}
            >
              Close
            </button>
          </div>
          {viewer.kind === "text" && (
            <pre className="files-viewer-text">{viewer.text}</pre>
          )}
          {viewer.kind === "image" && (
            <img
              className="files-viewer-image"
              src={viewer.imageSrc}
              alt={viewer.path}
            />
          )}
          {viewer.kind === "binary" && (
            <p className="files-viewer-binary">
              Binary file — use Download to save it locally.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

function toErrorState(e: unknown): ErrorState {
  if (e instanceof FileOpsError) return { code: e.code, message: e.message };
  if (e instanceof Error) return { code: "ClientError", message: e.message };
  return { code: "ClientError", message: String(e) };
}

function typeLabel(t: FileType): "DIR" | "FILE" | "LNK" | "OTHER" {
  switch (t) {
    case FileType.FILE_TYPE_DIRECTORY:
      return "DIR";
    case FileType.FILE_TYPE_REGULAR:
      return "FILE";
    case FileType.FILE_TYPE_SYMLINK:
      return "LNK";
    default:
      return "OTHER";
  }
}

function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MiB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(1)} GiB`;
}

function formatMtime(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleString();
}

function joinPath(base: string, name: string): string {
  if (base === "/") return `/${name}`;
  if (base.endsWith("/")) return `${base}${name}`;
  return `${base}/${name}`;
}

function buildBreadcrumbs(p: string): { label: string; path: string }[] {
  const trimmed = p.replace(/\/+$/, "");
  if (!trimmed || trimmed === "") {
    return [{ label: "/", path: "/" }];
  }
  const isAbs = trimmed.startsWith("/");
  const parts = trimmed.split("/").filter(Boolean);
  const crumbs: { label: string; path: string }[] = [];
  if (isAbs) crumbs.push({ label: "/", path: "/" });
  let cursor = isAbs ? "" : "";
  for (const part of parts) {
    cursor = isAbs ? `${cursor}/${part}` : cursor ? `${cursor}/${part}` : part;
    crumbs.push({ label: part, path: cursor || "/" });
  }
  return crumbs;
}

function isImage(name: string): boolean {
  return /\.(png|jpe?g|gif|webp|bmp|ico|svg)$/i.test(name);
}

function imageMimeFor(fmt: number): string {
  // ImageFormat enum: 0=original, 1=jpeg, 2=png, 3=webp
  switch (fmt) {
    case 1: return "image/jpeg";
    case 2: return "image/png";
    case 3: return "image/webp";
    default: return "image/png";
  }
}

function uint8ToBase64(bytes: Uint8Array): string {
  let s = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    s += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK) as unknown as number[]);
  }
  return btoa(s);
}
```

- [ ] **Step 4: Rerun tests**

```bash
pnpm --filter @ahand/hub-dashboard test -- device-files
```
Expected: both cases pass.

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/src/components/device-files.tsx apps/hub-dashboard/tests/device-files.test.tsx apps/hub-dashboard/package.json
git commit -m "feat(dashboard): add DeviceFiles list/navigate/view"
```

---

### Task 4: Mkdir / delete / download / upload

**Goal:** Round out the action set: create a subdirectory, delete an entry (confirm dialog + recursive toggle), download any file, upload a file (<= 1 MiB inline; larger shows the S3-not-implemented message).

**Files:**
- Modify: `apps/hub-dashboard/src/components/device-files.tsx`
- Modify: `apps/hub-dashboard/tests/device-files.test.tsx`

**Acceptance Criteria:**
- [ ] "New folder" action → inline input → calls `mkdir` → re-lists current directory
- [ ] Delete action on a listed entry → confirm dialog (`role="dialog"`, `aria-modal`) with "Recursive" checkbox → calls `deleteFile` → re-lists
- [ ] Download action on a listed file → calls `readBinary` → creates Blob URL → triggers `<a download>` click → URL revoked after download
- [ ] Upload: `<input type="file">` → if `file.size > 1_048_576` show "S3 path not implemented yet" error; else `writeFile` → re-list
- [ ] All new tests pass: mkdir happy, delete confirm-dialog, upload size guard

**Verify:** `pnpm --filter @ahand/hub-dashboard test -- device-files` → all tests pass

**Steps:**

- [ ] **Step 1: Add failing tests**

Append to `apps/hub-dashboard/tests/device-files.test.tsx` inside the same `describe("DeviceFiles", ...)`:

```tsx
it("creates a new directory via mkdir", async () => {
  // First list is the initial state.
  stubProto(
    FileResponse.fromPartial({
      requestId: "r",
      list: { entries: [], totalCount: 0, hasMore: false },
    }),
  );
  // Mkdir response.
  stubProto(
    FileResponse.fromPartial({
      requestId: "r",
      mkdir: { path: "/tmp/newfolder", alreadyExisted: false },
    }),
  );
  // Re-list after mkdir.
  stubProto(
    FileResponse.fromPartial({
      requestId: "r",
      list: {
        entries: [{ name: "newfolder", fileType: FileType.FILE_TYPE_DIRECTORY, size: 0, modifiedMs: 0 }],
        totalCount: 1,
        hasMore: false,
      },
    }),
  );

  const user = userEvent.setup();
  render(<DeviceFiles deviceId="dev-1" />);
  await user.click(screen.getByRole("button", { name: /open/i }));
  await screen.findByText(/empty directory/i);

  await user.click(screen.getByRole("button", { name: /new folder/i }));
  const nameInput = await screen.findByLabelText(/folder name/i);
  await user.type(nameInput, "newfolder");
  await user.click(screen.getByRole("button", { name: /create/i }));

  expect(await screen.findByText("newfolder")).toBeInTheDocument();
});

it("shows a confirm dialog before delete and respects Cancel", async () => {
  stubProto(
    FileResponse.fromPartial({
      requestId: "r",
      list: {
        entries: [
          { name: "old.txt", fileType: FileType.FILE_TYPE_REGULAR, size: 3, modifiedMs: 0 },
        ],
        totalCount: 1,
        hasMore: false,
      },
    }),
  );
  const user = userEvent.setup();
  render(<DeviceFiles deviceId="dev-1" />);
  await user.click(screen.getByRole("button", { name: /open/i }));
  await screen.findByText("old.txt");

  await user.click(screen.getByRole("button", { name: /delete old.txt/i }));
  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent(/delete/i);
  expect(screen.getByLabelText(/recursive/i)).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: /cancel/i }));
  expect(screen.queryByRole("dialog")).toBeNull();
  // fetch was only called once (the initial list) — no delete was issued.
  expect((globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);
});

it("rejects uploads larger than 1 MiB without calling the network", async () => {
  stubProto(
    FileResponse.fromPartial({
      requestId: "r",
      list: { entries: [], totalCount: 0, hasMore: false },
    }),
  );
  const user = userEvent.setup();
  render(<DeviceFiles deviceId="dev-1" />);
  await user.click(screen.getByRole("button", { name: /open/i }));
  await screen.findByText(/empty directory/i);

  const big = new File([new Uint8Array(1_048_577)], "big.bin", { type: "application/octet-stream" });
  const uploadInput = screen.getByLabelText(/upload file/i) as HTMLInputElement;
  await user.upload(uploadInput, big);

  const alert = await screen.findByRole("alert");
  expect(alert).toHaveTextContent(/S3/i);
  // One fetch from the initial list, none from the upload.
  expect((globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);
});
```

- [ ] **Step 2: Run tests, confirm they fail**

```bash
pnpm --filter @ahand/hub-dashboard test -- device-files
```
Expected: new tests FAIL.

- [ ] **Step 3: Extend `DeviceFiles` with actions**

Edit `apps/hub-dashboard/src/components/device-files.tsx`:

a) Add the new imports at the top:

```tsx
import { deleteFile, mkdir, readBinary, writeFile } from "@/lib/file-ops-client";
```

(Merge with existing imports from `@/lib/file-ops-client`.)

b) Inside the `DeviceFiles` function, add state and handlers before the `return`:

```tsx
const [mkdirName, setMkdirName] = useState<string | null>(null);
const [pendingDelete, setPendingDelete] = useState<{ name: string; recursive: boolean } | null>(null);
const [busy, setBusy] = useState<string | null>(null);

const handleMkdir = useCallback(async () => {
  if (mkdirName === null) return;
  const name = mkdirName.trim();
  if (!name) return;
  setBusy("mkdir");
  setError(null);
  try {
    await mkdir(deviceId, { path: joinPath(path, name), recursive: true });
    setMkdirName(null);
    await openDirectory(path);
  } catch (e) {
    setError(toErrorState(e));
  } finally {
    setBusy(null);
  }
}, [mkdirName, deviceId, path, openDirectory]);

const handleDelete = useCallback(async () => {
  if (!pendingDelete) return;
  const { name, recursive } = pendingDelete;
  setBusy("delete");
  setError(null);
  try {
    await deleteFile(deviceId, { path: joinPath(path, name), recursive });
    setPendingDelete(null);
    await openDirectory(path);
  } catch (e) {
    setError(toErrorState(e));
  } finally {
    setBusy(null);
  }
}, [pendingDelete, deviceId, path, openDirectory]);

const handleDownload = useCallback(
  async (name: string) => {
    setBusy(`download:${name}`);
    setError(null);
    try {
      const r = await readBinary(deviceId, { path: joinPath(path, name) });
      const blob = new Blob([r.content as unknown as Uint8Array], {
        type: "application/octet-stream",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = name;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
    } catch (e) {
      setError(toErrorState(e));
    } finally {
      setBusy(null);
    }
  },
  [deviceId, path],
);

const handleUpload = useCallback(
  async (file: File | null | undefined) => {
    if (!file) return;
    setBusy("upload");
    setError(null);
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      await writeFile(deviceId, { path: joinPath(path, file.name), content: bytes });
      await openDirectory(path);
    } catch (e) {
      setError(toErrorState(e));
    } finally {
      setBusy(null);
    }
  },
  [deviceId, path, openDirectory],
);
```

c) In the top action row (inside the `files-section` div that holds the path input), add a new sibling row under the existing form-row:

```tsx
<div className="files-actions-row">
  <button
    type="button"
    className="files-btn"
    onClick={() => setMkdirName("")}
    disabled={busy !== null}
  >
    New folder
  </button>
  <label className="files-btn files-upload-label">
    Upload file
    <input
      type="file"
      className="files-upload-input"
      aria-label="Upload file"
      onChange={(e) => handleUpload(e.target.files?.[0])}
    />
  </label>
</div>
{mkdirName !== null && (
  <div className="files-form-row">
    <label className="files-label" htmlFor="files-mkdir-input">
      Folder name
    </label>
    <input
      id="files-mkdir-input"
      className="files-input"
      value={mkdirName}
      onChange={(e) => setMkdirName(e.target.value)}
      onKeyDown={(e) => e.key === "Enter" && handleMkdir()}
    />
    <button
      type="button"
      className="files-btn files-btn-primary"
      onClick={handleMkdir}
      disabled={busy === "mkdir"}
    >
      Create
    </button>
    <button
      type="button"
      className="files-btn"
      onClick={() => setMkdirName(null)}
    >
      Cancel
    </button>
  </div>
)}
```

d) Inside the entry list, replace the entry `<button>` with a row that includes Download / Delete affordances for files:

```tsx
<li key={e.name} className="files-entry">
  <button
    type="button"
    className="files-entry-btn"
    onClick={() =>
      e.fileType === FileType.FILE_TYPE_DIRECTORY
        ? openDirectory(joinPath(path, e.name))
        : openFile(joinPath(path, e.name), e.name)
    }
    aria-label={`${typeLabel(e.fileType)} ${e.name}`}
  >
    <span className={`files-badge files-badge-${typeLabel(e.fileType).toLowerCase()}`}>
      {typeLabel(e.fileType)}
    </span>
    <span className="files-entry-name">{e.name}</span>
    <span className="files-entry-size">
      {e.fileType === FileType.FILE_TYPE_REGULAR ? humanSize(e.size) : ""}
    </span>
    <span className="files-entry-time">{formatMtime(e.modifiedMs)}</span>
  </button>
  <div className="files-entry-actions">
    {e.fileType === FileType.FILE_TYPE_REGULAR && (
      <button
        type="button"
        className="files-btn files-btn-sm"
        onClick={() => handleDownload(e.name)}
        disabled={busy === `download:${e.name}`}
        aria-label={`Download ${e.name}`}
      >
        Download
      </button>
    )}
    <button
      type="button"
      className="files-btn files-btn-sm files-btn-danger"
      onClick={() => setPendingDelete({ name: e.name, recursive: false })}
      disabled={busy === "delete"}
      aria-label={`Delete ${e.name}`}
    >
      Delete
    </button>
  </div>
</li>
```

e) Above the closing `</div>` of `.files-panel`, add the confirm dialog:

```tsx
{pendingDelete && (
  <div className="files-dialog-backdrop">
    <div
      className="files-dialog"
      role="dialog"
      aria-modal="true"
      aria-label={`Delete ${pendingDelete.name}`}
    >
      <h3 className="files-dialog-title">Delete {pendingDelete.name}?</h3>
      <label className="files-dialog-option">
        <input
          type="checkbox"
          checked={pendingDelete.recursive}
          onChange={(e) =>
            setPendingDelete({
              name: pendingDelete.name,
              recursive: e.target.checked,
            })
          }
        />
        Recursive (delete contents)
      </label>
      <div className="files-dialog-actions">
        <button
          type="button"
          className="files-btn"
          onClick={() => setPendingDelete(null)}
        >
          Cancel
        </button>
        <button
          type="button"
          className="files-btn files-btn-danger"
          onClick={handleDelete}
          disabled={busy === "delete"}
        >
          Delete
        </button>
      </div>
    </div>
  </div>
)}
```

- [ ] **Step 4: Run tests**

```bash
pnpm --filter @ahand/hub-dashboard test -- device-files
```
Expected: all 5 cases pass.

- [ ] **Step 5: Commit**

```bash
git add apps/hub-dashboard/src/components/device-files.tsx apps/hub-dashboard/tests/device-files.test.tsx
git commit -m "feat(dashboard): add mkdir/delete/download/upload to DeviceFiles"
```

---

### Task 5: Wire "Files" tab + CSS styles + final build/test verification

**Goal:** Expose `DeviceFiles` in the tabbed UI, add matching CSS, and confirm the whole Files tab works end-to-end in the Next.js production build.

**Files:**
- Modify: `apps/hub-dashboard/src/components/device-tabs.tsx`
- Modify: `apps/hub-dashboard/src/app/globals.css`

**Acceptance Criteria:**
- [ ] Device detail page shows a "Files" tab whenever `online === true` (same gate as Terminal)
- [ ] Clicking "Files" mounts `DeviceFiles` with the correct `deviceId`
- [ ] CSS classes `.files-panel`, `.files-section`, `.files-form-row`, `.files-input`, `.files-btn`, `.files-btn-primary`, `.files-btn-sm`, `.files-btn-danger`, `.files-list`, `.files-entry`, `.files-entry-btn`, `.files-entry-actions`, `.files-breadcrumbs`, `.files-breadcrumb`, `.files-error`, `.files-viewer`, `.files-viewer-text`, `.files-viewer-image`, `.files-dialog-backdrop`, `.files-dialog`, `.files-dialog-actions`, `.files-upload-label`, `.files-upload-input` are defined
- [ ] `pnpm --filter @ahand/hub-dashboard test` clean
- [ ] `pnpm --filter @ahand/hub-dashboard build` clean (Next.js production build)
- [ ] Coverage threshold (`vitest.config.ts`) not breached

**Verify:** `pnpm --filter @ahand/hub-dashboard test && pnpm --filter @ahand/hub-dashboard build`

**Steps:**

- [ ] **Step 1: Add Files tab to `device-tabs.tsx`**

Edit `apps/hub-dashboard/src/components/device-tabs.tsx`. Replace the component to add the Files branch:

```tsx
"use client";

import { useState } from "react";
import { DeviceJobsPanel } from "./device-jobs-panel";
import { DeviceTerminal } from "./device-terminal";
import { DeviceBrowser } from "./device-browser";
import { DeviceFiles } from "./device-files";

export function DeviceTabs({
  deviceId,
  online,
  capabilities,
}: {
  deviceId: string;
  online: boolean;
  capabilities: string[];
}) {
  const hasBrowser = online && capabilities.includes("browser");
  const [tab, setTab] = useState<"jobs" | "terminal" | "browser" | "files">(
    hasBrowser ? "browser" : online ? "terminal" : "jobs",
  );

  return (
    <article className="surface-panel device-tabs-panel">
      <div className="device-tabs-header">
        <button
          className={`device-tab ${tab === "jobs" ? "device-tab-active" : ""}`}
          onClick={() => setTab("jobs")}
        >
          Jobs
        </button>
        {online && (
          <button
            className={`device-tab ${tab === "terminal" ? "device-tab-active" : ""}`}
            onClick={() => setTab("terminal")}
          >
            Terminal
          </button>
        )}
        {hasBrowser && (
          <button
            className={`device-tab ${tab === "browser" ? "device-tab-active" : ""}`}
            onClick={() => setTab("browser")}
          >
            Browser
          </button>
        )}
        {online && (
          <button
            className={`device-tab ${tab === "files" ? "device-tab-active" : ""}`}
            onClick={() => setTab("files")}
          >
            Files
          </button>
        )}
      </div>

      {tab === "jobs" && (
        <div className="device-tab-content">
          <DeviceJobsPanel deviceId={deviceId} />
        </div>
      )}

      {tab === "terminal" && online && (
        <DeviceTerminal deviceId={deviceId} />
      )}

      {tab === "browser" && hasBrowser && (
        <DeviceBrowser deviceId={deviceId} />
      )}

      {tab === "files" && online && (
        <div className="device-tab-content">
          <DeviceFiles deviceId={deviceId} />
        </div>
      )}
    </article>
  );
}
```

- [ ] **Step 2: Add CSS styles**

Append to `apps/hub-dashboard/src/app/globals.css`:

```css
/* ---- Files tab ---- */
.files-panel {
  display: flex;
  flex-direction: column;
  gap: 18px;
  padding: 16px;
}
.files-section {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.files-form-row {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
}
.files-label {
  min-width: 96px;
  color: var(--muted);
  font-size: 13px;
}
.files-input {
  flex: 1 1 320px;
  min-width: 0;
  padding: 8px 12px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--surface-soft);
  color: var(--text);
}
.files-actions-row {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
.files-breadcrumbs {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
  font-size: 13px;
  color: var(--muted);
}
.files-breadcrumb {
  background: none;
  border: none;
  color: var(--accent);
  cursor: pointer;
  padding: 2px 4px;
  border-radius: 6px;
}
.files-breadcrumb:hover,
.files-breadcrumb:focus-visible {
  background: var(--surface-soft);
  outline: none;
}
.files-btn {
  padding: 6px 12px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--surface-soft);
  color: var(--text);
  font-size: 13px;
  cursor: pointer;
}
.files-btn:hover:not(:disabled) {
  background: rgba(148, 163, 184, 0.16);
}
.files-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.files-btn-primary {
  background: var(--accent-strong);
  color: #0b111b;
  border-color: var(--accent-strong);
}
.files-btn-sm {
  padding: 4px 8px;
  font-size: 12px;
}
.files-btn-danger {
  color: #fca5a5;
  border-color: rgba(252, 165, 165, 0.4);
}
.files-upload-label {
  position: relative;
  display: inline-flex;
  align-items: center;
  cursor: pointer;
}
.files-upload-input {
  position: absolute;
  inset: 0;
  opacity: 0;
  cursor: pointer;
}
.files-list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  border: 1px solid var(--border);
  border-radius: 12px;
  overflow: hidden;
}
.files-empty {
  padding: 16px;
  color: var(--muted);
  text-align: center;
}
.files-entry {
  display: flex;
  gap: 8px;
  padding: 6px 12px;
  align-items: center;
  border-bottom: 1px solid var(--border);
}
.files-entry:last-child {
  border-bottom: none;
}
.files-entry-btn {
  flex: 1 1 auto;
  display: grid;
  grid-template-columns: 52px 1fr auto auto;
  gap: 12px;
  align-items: center;
  background: none;
  border: none;
  color: var(--text);
  cursor: pointer;
  padding: 4px 0;
  text-align: left;
}
.files-entry-btn:focus-visible {
  outline: 2px solid var(--accent);
  border-radius: 6px;
}
.files-badge {
  font-size: 11px;
  padding: 2px 6px;
  border-radius: 6px;
  background: var(--surface-soft);
  color: var(--muted);
  text-align: center;
}
.files-badge-dir {
  color: var(--accent);
}
.files-entry-size,
.files-entry-time {
  color: var(--muted);
  font-size: 12px;
  white-space: nowrap;
}
.files-entry-actions {
  display: flex;
  gap: 4px;
}
.files-error {
  border: 1px solid rgba(252, 165, 165, 0.4);
  background: rgba(252, 165, 165, 0.08);
  color: #fecaca;
  border-radius: 10px;
  padding: 10px 12px;
  font-size: 13px;
}
.files-viewer {
  border: 1px solid var(--border);
  border-radius: 12px;
  padding: 12px;
  background: var(--surface-soft);
}
.files-viewer-header {
  display: flex;
  gap: 8px;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
}
.files-viewer-path {
  font-family: ui-monospace, monospace;
  font-size: 12px;
  color: var(--muted);
  word-break: break-all;
}
.files-viewer-text {
  font-family: ui-monospace, monospace;
  font-size: 12px;
  max-height: 60vh;
  overflow: auto;
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
}
.files-viewer-image {
  max-width: 100%;
  max-height: 60vh;
  display: block;
  margin: 0 auto;
}
.files-viewer-binary {
  color: var(--muted);
  font-size: 13px;
}
.files-dialog-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(8, 15, 29, 0.6);
  display: grid;
  place-items: center;
  z-index: 50;
}
.files-dialog {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: var(--radius-lg);
  padding: 20px;
  width: min(420px, 90vw);
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.files-dialog-title {
  margin: 0;
  font-size: 16px;
}
.files-dialog-option {
  display: flex;
  gap: 8px;
  align-items: center;
  font-size: 13px;
  color: var(--muted);
}
.files-dialog-actions {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}
```

- [ ] **Step 3: Run full dashboard test suite**

```bash
pnpm --filter @ahand/hub-dashboard test
```
Expected: all tests pass.

- [ ] **Step 4: Run production build**

```bash
pnpm --filter @ahand/hub-dashboard build
```
Expected: build completes with no errors. Warnings about static generation are acceptable; a *build error* is not.

- [ ] **Step 5: Manual UI smoke check (dev server)**

```bash
pnpm --filter @ahand/hub-dashboard dev
# Open http://localhost:1516/devices/<some-online-device-id>
# Confirm: Files tab appears → click → path input visible → open /tmp → list shows → click a text file → viewer opens
```
This is a manual verification step. If no online device is available in the local env, note that in the PR description ("manual smoke deferred — no dev device available") and rely on the component tests.

- [ ] **Step 6: Commit & push**

```bash
git add apps/hub-dashboard/src/components/device-tabs.tsx apps/hub-dashboard/src/app/globals.css
git commit -m "feat(dashboard): wire Files tab + styles"
git push -u origin feat/dashboard-files-tab
```

- [ ] **Step 7: Open PR**

```bash
gh pr create --base dev --title "feat(dashboard): Files tab on device detail page" --body "$(cat <<'EOF'
## Summary
- New **Files** tab on the device detail page (`/devices/{id}`), visible whenever the device is online.
- Operators can list directories, navigate via breadcrumbs, view text and image files, create folders, delete entries (with recursive toggle confirm dialog), download files, and upload files up to 1 MiB.
- Uploads >1 MiB show "S3 path not implemented yet" (tracking PR #22 per `docs/superpowers/specs/2026-04-12-device-file-operations-design.md`).
- Thin client helper `file-ops-client.ts` wraps protobuf encode/decode + fetch + the two error-envelope paths (HTTP 4xx JSON and `FileResponse.error`).
- New pass-through `POST` in `/api/proxy/[...path]/route.ts` forwards protobuf bodies to the hub unchanged.

## Flow checklist
- [x] list → navigate into folder
- [x] read text (`read_text`) → viewer renders
- [x] read image (`read_image`) → viewer renders
- [x] mkdir
- [x] delete (confirm dialog + recursive)
- [x] download (read_binary → Blob → `<a download>`)
- [x] upload (FullWrite inline ≤ 1 MiB; >1 MiB shows S3-not-implemented message)

## v1 boundaries
- No S3 large-file path — blocked on PR #22 (per spec "Large File S3 Transfer")
- No edit / chmod / copy / move / create_symlink UI (backend supports them — deliberately out of scope)
- No client-side path validation — hub `file_policy` is authoritative and its error message (e.g. `PolicyDenied: /etc/passwd is in dangerous_paths`) is surfaced inline

## Test coverage
- `tests/auth-server.test.ts` — POST proxy: protobuf body passthrough, 401 no-session, 503 no-base-url
- `tests/file-ops-client.test.ts` — encode/decode roundtrip, 4xx JSON envelope, proto-level `FileResponse.error`, >1 MiB guard
- `tests/device-files.test.tsx` — list happy, 4xx error envelope rendering, mkdir happy, delete confirm dialog (Cancel path), upload size guard

## Accessibility
- Path input has label
- List items are native `<button>`s with `aria-label`
- Delete dialog is `role="dialog"` with `aria-modal="true"`
- Error box is `role="alert"`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Self-Review Checklist

- [x] Every task has explicit file paths and exact code
- [x] Every test has the expected assertion codepath
- [x] Every `Verify:` is an executable command
- [x] No TODOs, no "similar to earlier task", no unexplained placeholders
- [x] Types, method names, CSS class names used in later tasks match earlier definitions (`listFiles`, `mkdir`, `deleteFile`, `readBinary`, `writeFile`, `FileOpsError`, `.files-*`)
- [x] Spec coverage: list/view/mkdir/delete/download/upload all mapped to Task 3 and Task 4; error envelope in Task 2 helper + Task 3 UI; accessibility in Task 3/4/5; build verification in Task 5
