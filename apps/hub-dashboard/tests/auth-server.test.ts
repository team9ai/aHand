// @vitest-environment node

import { NextRequest } from "next/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { POST as loginPost } from "@/app/api/auth/login/route";
import { GET as proxyGet } from "@/app/api/proxy/[...path]/route";
import { middleware } from "@/middleware";

const HUB_BASE_URL = "https://hub.example";

describe("hub dashboard auth server flow", () => {
  const originalBaseUrl = process.env.AHAND_HUB_BASE_URL;

  beforeEach(() => {
    process.env.AHAND_HUB_BASE_URL = HUB_BASE_URL;
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();

    if (originalBaseUrl === undefined) {
      delete process.env.AHAND_HUB_BASE_URL;
      return;
    }

    process.env.AHAND_HUB_BASE_URL = originalBaseUrl;
  });

  it("redirects unauthenticated dashboard requests to /login", () => {
    const request = new NextRequest("http://localhost/devices");
    const response = middleware(request);

    expect(response.headers.get("location")).toBe("http://localhost/login");
  });

  it("bypasses auth redirects for the login page and auth routes", () => {
    const loginPageResponse = middleware(new NextRequest("http://localhost/login"));
    const authRouteResponse = middleware(new NextRequest("http://localhost/api/auth/login"));

    expect(loginPageResponse.headers.get("x-middleware-next")).toBe("1");
    expect(authRouteResponse.headers.get("x-middleware-next")).toBe("1");
  });

  it("sets session cookies when the upstream login returns a token", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ token: "session-token" }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(fetchMock).toHaveBeenCalledWith(`${HUB_BASE_URL}/api/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });
    expect(response.cookies.get("ahand_hub_session")?.value).toBe("session-token");
    expect(response.cookies.get("ahand_hub_ws_token")?.value).toBe("session-token");
  });

  it("does not set cookies when the upstream login payload has no token", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ ok: true }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
    );

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.cookies.get("ahand_hub_session")).toBeUndefined();
    expect(response.cookies.get("ahand_hub_ws_token")).toBeUndefined();
  });

  it("returns 400 when the login request body is not valid JSON", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{invalid-json",
    });

    const response = await loginPost(request);
    const payload = await response.json();

    expect(response.status).toBe(400);
    expect(payload).toEqual({ error: "invalid_json" });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("forwards proxied GET requests with the session bearer token and accept header", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/proxy/api/devices", {
      headers: {
        accept: "text/event-stream",
        cookie: "ahand_hub_session=session-token",
      },
    });

    const response = await proxyGet(request, {
      params: Promise.resolve({ path: ["api", "devices"] }),
    });

    expect(fetchMock).toHaveBeenCalledWith(`${HUB_BASE_URL}/api/devices`, {
      headers: {
        authorization: "Bearer session-token",
        accept: "text/event-stream",
      },
      cache: "no-store",
    });
    expect(response.status).toBe(200);
  });
});
