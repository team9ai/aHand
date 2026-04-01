import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import DevicesPage from "@/app/(dashboard)/devices/page";
import DeviceDetailPage from "@/app/(dashboard)/devices/[id]/page";
import { Sidebar } from "@/components/sidebar";
import { getDevice, getDevices, getJobs } from "@/lib/api";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    getDevices: vi.fn(),
    getDevice: vi.fn(),
    getJobs: vi.fn(),
    withDashboardSession: actual.withDashboardSession,
  };
});

vi.mock("next/navigation", () => ({
  usePathname: () => "/devices",
}));

describe("devices surfaces", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("renders the device table filtered by status and search query", async () => {
    vi.mocked(getDevices).mockResolvedValue([
      {
        id: "device-1",
        hostname: "render-node",
        os: "linux",
        capabilities: ["exec", "gpu"],
        public_key: Array.from({ length: 32 }, (_, index) => index),
        version: "0.1.2",
        auth_method: "ed25519",
        online: true,
      },
      {
        id: "device-2",
        hostname: "ops-mac",
        os: "macos",
        capabilities: ["exec"],
        public_key: null,
        version: "0.1.2",
        auth_method: "ed25519",
        online: false,
      },
    ]);

    render(
      await DevicesPage({
        searchParams: Promise.resolve({ status: "online", q: "render" }),
      }),
    );

    expect(screen.getByRole("heading", { name: /devices/i })).toBeInTheDocument();
    expect(screen.getByText("render-node")).toBeInTheDocument();
    expect(screen.queryByText("ops-mac")).not.toBeInTheDocument();
    expect(screen.getByText(/^online$/i)).toBeInTheDocument();
  });

  it("renders an empty state when no devices match the filters", async () => {
    vi.mocked(getDevices).mockResolvedValue([]);

    render(await DevicesPage({ searchParams: Promise.resolve({ status: "offline" }) }));

    expect(screen.getByText(/no devices match the current filters/i)).toBeInTheDocument();
  });

  it("renders device detail metadata, fingerprint, and recent jobs", async () => {
    vi.mocked(getDevice).mockResolvedValue({
      id: "device-1",
      hostname: "render-node",
      os: "linux",
      capabilities: ["exec", "gpu"],
      public_key: Array.from({ length: 32 }, (_, index) => index),
      version: "0.1.2",
      auth_method: "ed25519",
      online: true,
    });
    vi.mocked(getJobs).mockResolvedValue([
      {
        id: "job-1",
        device_id: "device-1",
        tool: "render",
        args: ["scene.blend"],
        cwd: "/srv/work",
        timeout_ms: 30_000,
        status: "running",
      },
    ]);

    render(
      await DeviceDetailPage({
        params: Promise.resolve({ id: "device-1" }),
      }),
    );

    expect(screen.getByRole("heading", { name: /render-node/i })).toBeInTheDocument();
    expect(screen.getByText(/00010203/i)).toBeInTheDocument();
    expect(screen.getByText("gpu")).toBeInTheDocument();
    expect(screen.getByText("render")).toBeInTheDocument();
  });

  it("highlights the active sidebar destination", () => {
    render(<Sidebar />);

    expect(screen.getByRole("link", { name: /devices/i })).toHaveAttribute("data-active", "true");
  });
});
