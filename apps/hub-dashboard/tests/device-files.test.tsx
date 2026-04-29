import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Buffer } from "buffer";
import { FileRequest, FileResponse, FileType } from "@ahandai/proto";

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
            { name: "readme.txt", fileType: FileType.FILE_TYPE_FILE, size: 12, modifiedMs: 0 },
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

  it("shows a 'Binary file' placeholder when readText returns NUL-containing content", async () => {
    // Seed the initial list.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "data.bin", fileType: FileType.FILE_TYPE_FILE, size: 3, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );
    // readText succeeds but returns a line containing a NUL byte.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        readText: {
          lines: [
            { content: "\0garble", lineNumber: 1, truncated: false, remainingBytes: 0 },
          ],
          stopReason: 4,
          remainingBytes: 0,
          totalFileBytes: 3,
          totalLines: 1,
          detectedEncoding: "utf-8",
        },
      }),
    );

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /open/i }));
    const entry = await screen.findByRole("button", { name: /^file data\.bin$/i });
    await user.click(entry);

    expect(await screen.findByText(/binary file/i)).toBeInTheDocument();
  });

  it("renders an image via readImage when the entry has an image extension", async () => {
    // Seed the initial list.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "pic.png", fileType: FileType.FILE_TYPE_FILE, size: 4, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );
    // readImage returns a small PNG payload.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        readImage: {
          content: Buffer.from(new Uint8Array([0x89, 0x50, 0x4e, 0x47])),
          format: 2, // IMAGE_FORMAT_PNG
          width: 8,
          height: 8,
          originalBytes: 4,
          outputBytes: 4,
        },
      }),
    );

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /open/i }));
    const entry = await screen.findByRole("button", { name: /^file pic\.png$/i });
    await user.click(entry);

    const img = await screen.findByAltText("/tmp/pic.png");
    expect(img).toBeInstanceOf(HTMLImageElement);
    expect(img.getAttribute("src")).toMatch(/^data:image\/png;base64,/);
  });

  it("creates a new directory via mkdir", async () => {
    // Initial list: empty dir.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: { entries: [], totalCount: 0, hasMore: false },
      }),
    );
    // mkdir result.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        mkdir: { path: "/tmp/newfolder", alreadyExisted: false },
      }),
    );
    // Re-list after mkdir: newfolder now present.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "newfolder", fileType: FileType.FILE_TYPE_DIRECTORY, size: 0, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /^open$/i }));
    await screen.findByText(/empty directory/i);

    await user.click(screen.getByRole("button", { name: /new folder/i }));
    const nameInput = await screen.findByLabelText(/folder name/i);
    await user.type(nameInput, "newfolder");
    await user.click(screen.getByRole("button", { name: /^create$/i }));

    expect(await screen.findByText("newfolder")).toBeInTheDocument();
  });

  it("shows a confirm dialog before delete and respects Cancel", async () => {
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "old.txt", fileType: FileType.FILE_TYPE_FILE, size: 3, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );
    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /^open$/i }));
    await screen.findByText("old.txt");

    await user.click(screen.getByRole("button", { name: /^delete old\.txt$/i }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(/delete/i);
    expect(screen.getByLabelText(/recursive/i)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /^cancel$/i }));
    expect(screen.queryByRole("dialog")).toBeNull();
    // Only the initial list fetch — no delete call.
    expect((globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);
  });

  it("closes the delete dialog when Escape is pressed", async () => {
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "old.txt", fileType: FileType.FILE_TYPE_FILE, size: 3, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );
    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /^open$/i }));
    await screen.findByText("old.txt");

    await user.click(screen.getByRole("button", { name: /^delete old\.txt$/i }));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();

    await user.keyboard("{Escape}");

    expect(screen.queryByRole("dialog")).toBeNull();
    // Only the initial list fetch — Esc did not issue a network call.
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
    await user.click(screen.getByRole("button", { name: /^open$/i }));
    await screen.findByText(/empty directory/i);

    const big = new File([new Uint8Array(1_048_577)], "big.bin", { type: "application/octet-stream" });
    const uploadInput = screen.getByLabelText(/upload file/i) as HTMLInputElement;
    await user.upload(uploadInput, big);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/S3/i);
    expect((globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls).toHaveLength(1);
  });

  it("deletes an entry after confirm and re-lists the directory", async () => {
    // Initial list: one file.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "old.txt", fileType: FileType.FILE_TYPE_FILE, size: 3, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );
    // delete response.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        delete: { path: "/tmp/old.txt", mode: 1, itemsDeleted: 1 },
      }),
    );
    // Re-list: now empty.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: { entries: [], totalCount: 0, hasMore: false },
      }),
    );

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /^open$/i }));
    await screen.findByText("old.txt");

    await user.click(screen.getByRole("button", { name: /^delete old\.txt$/i }));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    // Click the Delete button INSIDE the dialog (aria-label is "Delete old.txt"
    // on the entry button; the dialog's Delete button has text "Delete").
    const dialog = screen.getByRole("dialog");
    const dialogDelete = within(dialog).getByRole("button", { name: /^delete$/i });
    await user.click(dialogDelete);

    expect(await screen.findByText(/empty directory/i)).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).toBeNull();

    // 3 network calls: initial list + delete + re-list.
    const fetchMock = globalThis.fetch as ReturnType<typeof vi.fn>;
    expect(fetchMock.mock.calls).toHaveLength(3);
    // The second call was the delete — decode its body to verify the
    // PERMANENT mode (value 1) is sent, not TRASH (0).
    const deleteBody = fetchMock.mock.calls[1][1].body as Uint8Array;
    const deleteReq = FileRequest.decode(deleteBody);
    expect(deleteReq.delete?.path).toBe("/tmp/old.txt");
    expect(deleteReq.delete?.mode).toBe(1); // DELETE_MODE_PERMANENT
  });

  it("uploads a small file, posts FullWrite.content, and re-lists", async () => {
    // Initial list: empty.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: { entries: [], totalCount: 0, hasMore: false },
      }),
    );
    // write response.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        write: { path: "/tmp/note.txt", action: 0, bytesWritten: 5, finalSize: 5 },
      }),
    );
    // Re-list: file now present.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "note.txt", fileType: FileType.FILE_TYPE_FILE, size: 5, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /^open$/i }));
    await screen.findByText(/empty directory/i);

    // Directory-traversal filename — the component must strip to basename
    // before sending the target path.
    const smallBytes = new Uint8Array([1, 2, 3, 4, 5]);
    const small = new File([smallBytes], "../../evil.txt", {
      type: "application/octet-stream",
    });
    // jsdom's File implementation does not provide arrayBuffer(); shim it.
    Object.defineProperty(small, "arrayBuffer", {
      value: async () => smallBytes.buffer.slice(
        smallBytes.byteOffset,
        smallBytes.byteOffset + smallBytes.byteLength,
      ),
    });
    const uploadInput = screen.getByLabelText(/upload file/i) as HTMLInputElement;
    await user.upload(uploadInput, small);

    expect(await screen.findByText("note.txt")).toBeInTheDocument();

    const fetchMock = globalThis.fetch as ReturnType<typeof vi.fn>;
    // 3 calls: initial list + write + re-list.
    expect(fetchMock.mock.calls).toHaveLength(3);
    const writeBody = fetchMock.mock.calls[1][1].body as Uint8Array;
    const writeReq = FileRequest.decode(writeBody);
    // basename("../../evil.txt") → "evil.txt"; joined with "/tmp" → "/tmp/evil.txt"
    expect(writeReq.write?.path).toBe("/tmp/evil.txt");
    expect(writeReq.write?.createParents).toBe(true);
    expect(writeReq.write?.fullWrite?.content).toBeDefined();
  });

  it("downloads a file by calling readBinary and triggering an anchor click", async () => {
    // Initial list: one file.
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        list: {
          entries: [
            { name: "log.txt", fileType: FileType.FILE_TYPE_FILE, size: 4, modifiedMs: 0 },
          ],
          totalCount: 1,
          hasMore: false,
        },
      }),
    );
    // readBinary response.
    const payload = Buffer.from(new Uint8Array([0x68, 0x69, 0x21, 0x0a])); // "hi!\n"
    stubProto(
      FileResponse.fromPartial({
        requestId: "r",
        readBinary: {
          content: payload,
          byteOffset: 0,
          bytesRead: 4,
          totalFileBytes: 4,
          remainingBytes: 0,
        },
      }),
    );

    // jsdom doesn't implement URL.createObjectURL / revokeObjectURL; stub them.
    const createObjectURL = vi.fn(() => "blob:mock-url");
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", {
      ...URL,
      createObjectURL,
      revokeObjectURL,
    });

    // Spy on anchor click so we can verify the download was triggered.
    const clickSpy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});

    const user = userEvent.setup();
    render(<DeviceFiles deviceId="dev-1" />);
    await user.click(screen.getByRole("button", { name: /^open$/i }));
    await screen.findByText("log.txt");

    await user.click(screen.getByRole("button", { name: /^download log\.txt$/i }));

    // Anchor click fired → download triggered.
    await vi.waitFor(() => {
      expect(clickSpy).toHaveBeenCalled();
    });
    expect(createObjectURL).toHaveBeenCalled();
    // revokeObjectURL is scheduled via setTimeout(0) — flush the task.
    await vi.waitFor(() => {
      expect(revokeObjectURL).toHaveBeenCalledWith("blob:mock-url");
    });

    // 2 network calls: initial list + readBinary.
    const fetchMock = globalThis.fetch as ReturnType<typeof vi.fn>;
    expect(fetchMock.mock.calls).toHaveLength(2);
    const readBody = fetchMock.mock.calls[1][1].body as Uint8Array;
    const readReq = FileRequest.decode(readBody);
    expect(readReq.readBinary?.path).toBe("/tmp/log.txt");
    // maxBytes must NOT be clamped to the 1 MiB write cap — should be unset
    // so the daemon uses its policy budget and we never silently truncate.
    expect(readReq.readBinary?.maxBytes ?? 0).toBe(0);

    clickSpy.mockRestore();
  });
});
