import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
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
