// @vitest-environment node

import { NextRequest } from "next/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { POST as loginPost } from "@/app/api/auth/login/route";
import { POST as logoutPost } from "@/app/api/auth/logout/route";
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

  it("returns 401 for unauthenticated proxy requests in middleware", async () => {
    const request = new NextRequest("http://localhost/api/proxy/api/devices");
    const response = middleware(request);

    expect(response.status).toBe(401);
    await expect(response.json()).resolves.toEqual({ error: "unauthorized" });
  });

  it("bypasses auth redirects for the login page and auth routes", () => {
    const loginPageResponse = middleware(new NextRequest("http://localhost/login"));
    const authRouteResponse = middleware(new NextRequest("http://localhost/api/auth/login"));

    expect(loginPageResponse.headers.get("x-middleware-next")).toBe("1");
    expect(authRouteResponse.headers.get("x-middleware-next")).toBe("1");
  });

  it("allows authenticated requests through middleware", () => {
    const request = new NextRequest("http://localhost/", {
      headers: { cookie: "ahand_hub_session=session-token" },
    });

    const response = middleware(request);

    expect(response.headers.get("x-middleware-next")).toBe("1");
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
    expect(response.cookies.get("ahand_hub_ws_token")).toBeUndefined();
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

  it("returns 503 when the hub login upstream is unavailable", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("fetch failed")));

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);
    const payload = await response.json();

    expect(response.status).toBe(503);
    expect(payload).toEqual({ error: "hub_unavailable" });
    expect(response.cookies.get("ahand_hub_session")).toBeUndefined();
  });

  it("redirects logout POST requests with see-other semantics", async () => {
    const response = await logoutPost();

    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe("/login");
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

  it("forwards proxy query strings to the upstream URL", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ ok: true }), {
        status: 200,
        headers: { "content-type": "application/json" },
      }),
    );

    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/proxy/api/devices?cursor=abc&limit=20", {
      headers: {
        cookie: "ahand_hub_session=session-token",
      },
    });

    await proxyGet(request, {
      params: Promise.resolve({ path: ["api", "devices"] }),
    });

    expect(fetchMock).toHaveBeenCalledWith(`${HUB_BASE_URL}/api/devices?cursor=abc&limit=20`, {
      headers: {
        authorization: "Bearer session-token",
        accept: "application/json",
      },
      cache: "no-store",
    });
  });

  it("returns 401 when the proxy route is invoked without a session cookie", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/proxy/api/devices");

    const response = await proxyGet(request, {
      params: Promise.resolve({ path: ["api", "devices"] }),
    });

    expect(response.status).toBe(401);
    await expect(response.json()).resolves.toEqual({ error: "unauthorized" });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("preserves upstream proxy error statuses", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ error: "hub_unavailable" }), {
          status: 502,
          headers: { "content-type": "application/json" },
        }),
      ),
    );

    const request = new NextRequest("http://localhost/api/proxy/api/devices", {
      headers: {
        cookie: "ahand_hub_session=session-token",
      },
    });

    const response = await proxyGet(request, {
      params: Promise.resolve({ path: ["api", "devices"] }),
    });

    expect(response.status).toBe(502);
    await expect(response.json()).resolves.toEqual({ error: "hub_unavailable" });
  });

  it("returns 503 when proxy base URL config is missing", async () => {
    delete process.env.AHAND_HUB_BASE_URL;
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/proxy/api/devices", {
      headers: {
        cookie: "ahand_hub_session=session-token",
      },
    });

    const response = await proxyGet(request, {
      params: Promise.resolve({ path: ["api", "devices"] }),
    });

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({ error: "hub_unavailable" });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("returns 503 when the proxy upstream fetch rejects", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("fetch failed")));

    const request = new NextRequest("http://localhost/api/proxy/api/devices", {
      headers: {
        cookie: "ahand_hub_session=session-token",
      },
    });

    const response = await proxyGet(request, {
      params: Promise.resolve({ path: ["api", "devices"] }),
    });

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({ error: "hub_unavailable" });
  });
});
