import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import DevicesPage from "@/app/(dashboard)/devices/page";
import DeviceDetailPage from "@/app/(dashboard)/devices/[id]/page";
import { Sidebar } from "@/components/sidebar";
import { getDevice, getDevices } from "@/lib/api";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    getDevices: vi.fn(),
    getDevice: vi.fn(),
    withDashboardSession: actual.withDashboardSession,
  };
});

const { redirectMock } = vi.hoisted(() => ({
  redirectMock: vi.fn((path: string) => {
    throw new Error(`REDIRECT:${path}`);
  }),
}));

vi.mock("next/navigation", () => ({
  usePathname: () => "/devices",
  redirect: redirectMock,
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

  it("renders device detail metadata, fingerprint, and capabilities", async () => {
    // The jobs panel is no longer rendered server-side — DeviceJobsPanel
    // (a client component) fetches its own jobs via `fetch`. Job-list
    // assertions live in `device-jobs-panel.test.tsx` to avoid coupling
    // this server-render test to the client tab's data flow.
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

    render(
      await DeviceDetailPage({
        params: Promise.resolve({ id: "device-1" }),
      }),
    );

    expect(screen.getByRole("heading", { name: /render-node/i })).toBeInTheDocument();
    expect(screen.getByText(/00010203/i)).toBeInTheDocument();
    expect(screen.getByText("gpu")).toBeInTheDocument();
  });

  it("highlights the active sidebar destination", () => {
    render(<Sidebar />);

    expect(screen.getByRole("link", { name: /devices/i })).toHaveAttribute("data-active", "true");
  });

  it("renders device not found when the device does not exist", async () => {
    vi.mocked(getDevice).mockResolvedValue(null);

    render(
      await DeviceDetailPage({
        params: Promise.resolve({ id: "device-404" }),
      }),
    );

    expect(screen.getByRole("heading", { name: /device not found/i })).toBeInTheDocument();
  });

  it("redirects to login when the devices API returns an auth error", async () => {
    vi.mocked(getDevices).mockRejectedValue(new Error("api_401"));

    await expect(
      DevicesPage({ searchParams: Promise.resolve({}) }),
    ).rejects.toThrow("REDIRECT:/login");
  });
});
