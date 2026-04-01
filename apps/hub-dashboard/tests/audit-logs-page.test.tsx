import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import AuditLogsPage from "@/app/(dashboard)/audit-logs/page";
import { getAuditLogs } from "@/lib/api";

vi.mock("@/lib/api", () => ({
  getAuditLogs: vi.fn(),
}));

describe("audit logs page", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("renders audit entries and expandable structured detail", async () => {
    vi.mocked(getAuditLogs).mockResolvedValue([
      {
        timestamp: "2026-04-02T09:15:00Z",
        action: "job.failed",
        resource_type: "job",
        resource_id: "job-9",
        actor: "device:device-1",
        detail: { error: "timeout" },
      },
    ]);

    render(await AuditLogsPage({ searchParams: Promise.resolve({ action: "job.failed" }) }));

    expect(screen.getByRole("heading", { name: /audit logs/i })).toBeInTheDocument();
    expect(screen.getByText(/job.failed/i)).toBeInTheDocument();
    expect(screen.getByText(/timeout/i)).toBeInTheDocument();
  });

  it("renders an empty state when there are no matching audit entries", async () => {
    vi.mocked(getAuditLogs).mockResolvedValue([]);

    render(await AuditLogsPage({ searchParams: Promise.resolve({ resource: "device-404" }) }));

    expect(screen.getByText(/no audit entries match the current filters/i)).toBeInTheDocument();
  });
});
