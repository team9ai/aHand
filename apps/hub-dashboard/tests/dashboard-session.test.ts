// @vitest-environment node

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import DashboardLayout from "@/app/(dashboard)/layout";
import { verifyDashboardSession } from "@/lib/dashboard-session";

const cookiesMock = vi.hoisted(() => vi.fn());
const redirectMock = vi.hoisted(() =>
  vi.fn((target: string) => {
    throw new Error(`redirect:${target}`);
  }),
);

vi.mock("next/headers", () => ({
  cookies: cookiesMock,
}));

vi.mock("next/navigation", () => ({
  redirect: redirectMock,
}));

describe("dashboard shell session verification", () => {
  const originalBaseUrl = process.env.AHAND_HUB_BASE_URL;
  const requestCookies = {
    get: vi.fn(),
  };

  beforeEach(() => {
    process.env.AHAND_HUB_BASE_URL = "https://hub.example";
    cookiesMock.mockReturnValue(requestCookies);
    requestCookies.get.mockReset();
    vi.stubGlobal("fetch", vi.fn());
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();

    if (originalBaseUrl === undefined) {
      delete process.env.AHAND_HUB_BASE_URL;
    } else {
      process.env.AHAND_HUB_BASE_URL = originalBaseUrl;
    }
  });

  it("returns null when the session cookie is missing", async () => {
    requestCookies.get.mockReturnValue(undefined);

    await expect(verifyDashboardSession()).resolves.toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("rejects invalid sessions before rendering the dashboard shell", async () => {
    requestCookies.get.mockReturnValue({ value: "stale-session" });
    vi.mocked(fetch).mockResolvedValue(new Response(null, { status: 401 }));

    await expect(DashboardLayout({ children: "child" })).rejects.toThrow("redirect:/login");
    expect(fetch).toHaveBeenCalledWith("https://hub.example/api/auth/verify", {
      headers: {
        authorization: "Bearer stale-session",
      },
      cache: "no-store",
    });
  });

  it("allows the dashboard shell when verification succeeds", async () => {
    requestCookies.get.mockReturnValue({ value: "session-token" });
    vi.mocked(fetch).mockResolvedValue(new Response(null, { status: 200 }));

    const element = await DashboardLayout({ children: "child" });

    expect(element.props.className).toBe("dashboard-shell");
    expect(fetch).toHaveBeenCalledWith("https://hub.example/api/auth/verify", {
      headers: {
        authorization: "Bearer session-token",
      },
      cache: "no-store",
    });
  });

  it("returns null when the hub base URL is not configured", async () => {
    delete process.env.AHAND_HUB_BASE_URL;
    requestCookies.get.mockReturnValue({ value: "session-token" });

    await expect(verifyDashboardSession()).resolves.toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("returns null when the verification fetch throws a network error", async () => {
    requestCookies.get.mockReturnValue({ value: "session-token" });
    vi.mocked(fetch).mockRejectedValue(new TypeError("fetch failed"));

    await expect(verifyDashboardSession()).resolves.toBeNull();
  });

  it("returns null for server error responses from the hub", async () => {
    requestCookies.get.mockReturnValue({ value: "session-token" });
    vi.mocked(fetch).mockResolvedValue(new Response(null, { status: 500 }));

    await expect(verifyDashboardSession()).resolves.toBeNull();
  });

  it("redirects to login when verification fetch throws", async () => {
    requestCookies.get.mockReturnValue({ value: "session-token" });
    vi.mocked(fetch).mockRejectedValue(new TypeError("fetch failed"));

    await expect(DashboardLayout({ children: "child" })).rejects.toThrow("redirect:/login");
  });
});
