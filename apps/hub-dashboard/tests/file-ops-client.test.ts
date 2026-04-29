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
