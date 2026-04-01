import { render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import JobsPage from "@/app/(dashboard)/jobs/page";
import JobDetailPage from "@/app/(dashboard)/jobs/[id]/page";
import { JobOutputViewer } from "@/components/job-output-viewer";
import { useJobOutput } from "@/hooks/use-job-output";
import { getAuditLogs, getJob, getJobs } from "@/lib/api";

vi.mock("@/lib/api", () => ({
  getJobs: vi.fn(),
  getJob: vi.fn(),
  getAuditLogs: vi.fn(),
}));

vi.mock("@/hooks/use-job-output", () => ({
  useJobOutput: vi.fn(),
}));

describe("jobs surfaces", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("renders the job list filtered by status and device", async () => {
    vi.mocked(getJobs).mockResolvedValue([
      {
        id: "job-1",
        device_id: "device-1",
        tool: "echo",
        args: ["hello"],
        cwd: null,
        env: {},
        timeout_ms: 30_000,
        status: "Running",
        requested_by: "operator",
      },
      {
        id: "job-2",
        device_id: "device-2",
        tool: "sleep",
        args: ["30"],
        cwd: null,
        env: {},
        timeout_ms: 30_000,
        status: "Finished",
        requested_by: "operator",
      },
    ]);

    render(
      await JobsPage({
        searchParams: Promise.resolve({ status: "running", device: "device-1" }),
      }),
    );

    expect(screen.getByText("echo")).toBeInTheDocument();
    expect(screen.queryByText("sleep")).not.toBeInTheDocument();
    expect(within(screen.getByRole("table")).getByText(/^running$/i)).toBeInTheDocument();
  });

  it("renders an empty jobs state when the list is empty", async () => {
    vi.mocked(getJobs).mockResolvedValue([]);

    render(await JobsPage({ searchParams: Promise.resolve({}) }));

    expect(screen.getByText(/no jobs found for the current filters/i)).toBeInTheDocument();
  });

  it("renders job detail metadata and timeline events", async () => {
    vi.mocked(getJob).mockResolvedValue({
      id: "job-1",
      device_id: "device-1",
      tool: "echo",
      args: ["hello"],
      cwd: "/tmp",
      env: {},
      timeout_ms: 30_000,
      status: "Finished",
      requested_by: "operator",
    });
    vi.mocked(getAuditLogs).mockResolvedValue([
      {
        timestamp: "2026-04-02T09:12:00Z",
        action: "job.created",
        resource_type: "job",
        resource_id: "job-1",
        actor: "operator",
        detail: { status: "pending" },
      },
      {
        timestamp: "2026-04-02T09:13:00Z",
        action: "job.finished",
        resource_type: "job",
        resource_id: "job-1",
        actor: "device:device-1",
        detail: { status: "finished" },
      },
    ]);
    vi.mocked(useJobOutput).mockReturnValue({
      entries: [
        { type: "stdout", text: "hello" },
        { type: "finished", text: "Command exited with code 0" },
      ],
      status: "complete",
      error: null,
    });

    render(
      await JobDetailPage({
        params: Promise.resolve({ id: "job-1" }),
      }),
    );

    expect(screen.getByRole("heading", { name: /job job-1/i })).toBeInTheDocument();
    expect(screen.getByText(/job.created/i)).toBeInTheDocument();
    expect(screen.getByText(/job.finished/i)).toBeInTheDocument();
    expect(screen.getByText("hello")).toBeInTheDocument();
  });

  it("renders terminal output entries from the SSE hook", () => {
    vi.mocked(useJobOutput).mockReturnValue({
      entries: [
        { type: "stdout", text: "starting" },
        { type: "stderr", text: "warning" },
      ],
      status: "streaming",
      error: null,
    });

    render(<JobOutputViewer jobId="job-1" />);

    expect(screen.getByText("starting")).toBeInTheDocument();
    expect(screen.getByText("warning")).toBeInTheDocument();
  });
});
