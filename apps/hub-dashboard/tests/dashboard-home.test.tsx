import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import DashboardHomePage from "@/app/(dashboard)/page";
import { getAuditLogs, getDashboardStats } from "@/lib/api";

const { redirectMock } = vi.hoisted(() => ({
  redirectMock: vi.fn((path: string) => {
    throw new Error(`REDIRECT:${path}`);
  }),
}));

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    getDashboardStats: vi.fn(),
    getAuditLogs: vi.fn(),
    withDashboardSession: actual.withDashboardSession,
  };
});

vi.mock("next/navigation", () => ({
  redirect: redirectMock,
}));

describe("dashboard overview page", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("renders the live overview counts and recent activity", async () => {
    vi.mocked(getDashboardStats).mockResolvedValue({
      online_devices: 3,
      offline_devices: 1,
      running_jobs: 2,
    });
    vi.mocked(getAuditLogs).mockResolvedValue([
      {
        timestamp: "2026-04-02T09:15:00Z",
        action: "job.finished",
        resource_type: "job",
        resource_id: "job-1",
        actor: "device:device-1",
        detail: { status: "finished" },
      },
    ]);

    render(await DashboardHomePage());

    expect(screen.getByRole("heading", { name: /overview/i })).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();
    expect(screen.getByText("1")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
    expect(screen.getByText(/job.finished/i)).toBeInTheDocument();
  });

  it("renders an empty activity state when the hub has no recent events", async () => {
    vi.mocked(getDashboardStats).mockResolvedValue({
      online_devices: 0,
      offline_devices: 1,
      running_jobs: 0,
    });
    vi.mocked(getAuditLogs).mockResolvedValue([]);

    render(await DashboardHomePage());

    expect(screen.getByText(/no recent activity yet/i)).toBeInTheDocument();
  });

  it("redirects to login when the dashboard session is invalid", async () => {
    vi.mocked(getDashboardStats).mockRejectedValue(new Error("api_401"));

    await expect(DashboardHomePage()).rejects.toThrow("REDIRECT:/login");
  });
});
