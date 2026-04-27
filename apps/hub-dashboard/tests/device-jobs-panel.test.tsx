import { render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DeviceJobsPanel } from "@/components/device-jobs-panel";

// DeviceJobsPanel is a client component that fetches jobs via raw `fetch`
// (not the `getJobs` lib helper) so we stub `fetch` globally. These tests
// used to live inside `devices-page.test.tsx`, but the page is a server
// component that no longer renders the panel synchronously — the assertion
// belongs here, where we can control the panel's data source directly.
describe("DeviceJobsPanel", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it("renders active and recent jobs returned by the proxy fetch", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify([
          {
            id: "job-1",
            tool: "render",
            args: ["scene.blend"],
            status: "running",
          },
          {
            id: "job-2",
            tool: "git",
            args: ["status"],
            status: "finished",
            exit_code: 0,
          },
        ]),
        { status: 200, headers: { "content-type": "application/json" } },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(<DeviceJobsPanel deviceId="device-1" />);

    // Active panel — running job rendered as a tool link.
    expect(await screen.findByRole("link", { name: "render" })).toBeInTheDocument();
    // Recent panel — finished job rendered alongside its exit code.
    expect(await screen.findByRole("link", { name: "git" })).toBeInTheDocument();
    expect(screen.getByText(/exit 0/)).toBeInTheDocument();

    // Sanity: hit the proxy URL with the scoped device_id.
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/proxy/api/jobs?device_id=device-1",
      expect.objectContaining({ cache: "no-store" }),
    );
  });

  it("surfaces an inline error when the proxy returns a non-2xx status", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("nope", { status: 500 })),
    );

    render(<DeviceJobsPanel deviceId="device-1" />);

    expect(await screen.findByText(/failed to load jobs \(500\)/i)).toBeInTheDocument();
  });

  it("shows a loading state until the first fetch resolves", async () => {
    let resolve: ((value: Response) => void) | undefined;
    const pending = new Promise<Response>((r) => {
      resolve = r;
    });
    vi.stubGlobal("fetch", vi.fn().mockReturnValue(pending));

    render(<DeviceJobsPanel deviceId="device-1" />);

    // Before fetch resolves, the panel is still in its initial null state.
    expect(screen.getByText(/loading jobs/i)).toBeInTheDocument();

    resolve?.(
      new Response(JSON.stringify([]), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    // After resolution, the panel transitions to the empty-state.
    expect(await screen.findByText(/no active jobs/i)).toBeInTheDocument();
  });
});
