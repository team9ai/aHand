// @vitest-environment node

import { NextRequest } from "next/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { POST as loginPost } from "@/app/api/auth/login/route";
import { POST as logoutPost } from "@/app/api/auth/logout/route";
import { GET as proxyGet } from "@/app/api/proxy/[...path]/route";
import { middleware } from "@/middleware";

describe("POST /api/proxy/*", () => {
  const HUB_BASE_URL = "http://hub.internal:8080";

  beforeEach(() => {
    vi.stubEnv("AHAND_HUB_BASE_URL", HUB_BASE_URL);
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
  });

  it("forwards protobuf bodies with content-type preserved", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(new Uint8Array([0x0a, 0x03, 0x66, 0x6f, 0x6f]), {
        status: 200,
        headers: { "content-type": "application/x-protobuf" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { POST: proxyPost } = await import("@/app/api/proxy/[...path]/route");
    const bodyBytes = new Uint8Array([0x08, 0x01, 0x12, 0x04, 0x74, 0x65, 0x73, 0x74]);
    const request = new NextRequest(
      "http://localhost/api/proxy/api/devices/dev-1/files",
      {
        method: "POST",
        headers: {
          "content-type": "application/x-protobuf",
          accept: "application/x-protobuf",
          cookie: "ahand_hub_session=session-token",
        },
        body: bodyBytes,
      },
    );

    const response = await proxyPost(request, {
      params: Promise.resolve({ path: ["api", "devices", "dev-1", "files"] }),
    });

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [calledUrl, calledInit] = fetchMock.mock.calls[0];
    expect(calledUrl).toBe(`${HUB_BASE_URL}/api/devices/dev-1/files`);
    expect(calledInit.method).toBe("POST");
    expect(calledInit.headers).toMatchObject({
      authorization: "Bearer session-token",
      "content-type": "application/x-protobuf",
      accept: "application/x-protobuf",
    });
    const forwarded = new Uint8Array(
      calledInit.body instanceof ArrayBuffer
        ? calledInit.body
        : (calledInit.body as Uint8Array).buffer,
    );
    expect(Array.from(forwarded)).toEqual(Array.from(bodyBytes));
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("application/x-protobuf");
  });

  it("returns 401 JSON envelope when session cookie is missing", async () => {
    const { POST: proxyPost } = await import("@/app/api/proxy/[...path]/route");
    const request = new NextRequest("http://localhost/api/proxy/api/devices/x/files", {
      method: "POST",
      headers: { "content-type": "application/x-protobuf" },
      body: new Uint8Array([0x00]),
    });
    const response = await proxyPost(request, {
      params: Promise.resolve({ path: ["api", "devices", "x", "files"] }),
    });
    expect(response.status).toBe(401);
    const body = await response.json();
    expect(body.error.code).toBe("unauthorized");
  });

  it("returns 503 when AHAND_HUB_BASE_URL is missing", async () => {
    vi.unstubAllEnvs();
    const { POST: proxyPost } = await import("@/app/api/proxy/[...path]/route");
    const request = new NextRequest("http://localhost/api/proxy/api/devices/x/files", {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
        cookie: "ahand_hub_session=session-token",
      },
      body: new Uint8Array([0x00]),
    });
    const response = await proxyPost(request, {
      params: Promise.resolve({ path: ["api", "devices", "x", "files"] }),
    });
    expect(response.status).toBe(503);
    const body = await response.json();
    expect(body.error.code).toBe("hub_unavailable");
  });

  it("returns 503 when the upstream fetch throws", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockRejectedValue(new Error("ECONNREFUSED")),
    );
    const { POST: proxyPost } = await import("@/app/api/proxy/[...path]/route");
    const request = new NextRequest("http://localhost/api/proxy/api/devices/x/files", {
      method: "POST",
      headers: {
        "content-type": "application/x-protobuf",
        cookie: "ahand_hub_session=session-token",
      },
      body: new Uint8Array([0x00]),
    });
    const response = await proxyPost(request, {
      params: Promise.resolve({ path: ["api", "devices", "x", "files"] }),
    });
    expect(response.status).toBe(503);
    const body = await response.json();
    expect(body.error.code).toBe("hub_unavailable");
  });
});

const HUB_BASE_URL = "https://hub.example";

describe("hub dashboard auth server flow", () => {
  const originalBaseUrl = process.env.AHAND_HUB_BASE_URL;
  const originalNodeEnv = process.env.NODE_ENV;

  beforeEach(() => {
    process.env.AHAND_HUB_BASE_URL = HUB_BASE_URL;
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();

    if (originalBaseUrl === undefined) {
      delete process.env.AHAND_HUB_BASE_URL;
    } else {
      process.env.AHAND_HUB_BASE_URL = originalBaseUrl;
    }

    if (originalNodeEnv === undefined) {
      delete process.env.NODE_ENV;
    } else {
      process.env.NODE_ENV = originalNodeEnv;
    }
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
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "unauthorized",
        message: "Sign in required.",
      },
    });
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

  it("does not mark session cookies as secure for plain-http login requests", async () => {
    process.env.NODE_ENV = "production";
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ token: "session-token" }), {
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

    expect(response.headers.get("set-cookie")).not.toContain("Secure");
  });

  it("marks session cookies as secure when the login request is forwarded over https", async () => {
    process.env.NODE_ENV = "production";
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ token: "session-token" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
    );

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-forwarded-proto": "https",
      },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.headers.get("set-cookie")).toContain("Secure");
  });

  it("does not mark session cookies as secure when the trusted forwarded proto is plain http", async () => {
    process.env.NODE_ENV = "production";
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ token: "session-token" }), {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      ),
    );

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-forwarded-proto": "http, https",
      },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.headers.get("set-cookie")).not.toContain("Secure");
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
    expect(payload).toEqual({
      error: {
        code: "invalid_json",
        message: "Request body must be valid JSON.",
      },
    });
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
    expect(payload).toEqual({
      error: {
        code: "hub_unavailable",
        message: "Unable to reach the hub right now.",
      },
    });
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

  it("forwards last-event-id for proxied SSE reconnects", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response("event: stdout\ndata: hello\n\n", {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      }),
    );

    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/proxy/api/jobs/job-1/output", {
      headers: {
        accept: "text/event-stream",
        "last-event-id": "7",
        cookie: "ahand_hub_session=session-token",
      },
    });

    const response = await proxyGet(request, {
      params: Promise.resolve({ path: ["api", "jobs", "job-1", "output"] }),
    });

    expect(fetchMock).toHaveBeenCalledWith(`${HUB_BASE_URL}/api/jobs/job-1/output`, {
      headers: {
        authorization: "Bearer session-token",
        accept: "text/event-stream",
        "last-event-id": "7",
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
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "unauthorized",
        message: "Sign in required.",
      },
    });
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
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "hub_unavailable",
        message: "Unable to reach the hub right now.",
      },
    });
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
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "hub_unavailable",
        message: "Unable to reach the hub right now.",
      },
    });
  });

  it("rewrites authenticated dashboard websocket requests to the hub at runtime", () => {
    const request = new NextRequest("http://localhost/ws/dashboard", {
      headers: { cookie: "ahand_hub_session=session-token" },
    });

    const response = middleware(request);

    expect(response.headers.get("x-middleware-rewrite")).toBe(`${HUB_BASE_URL}/ws/dashboard`);
  });

  it("rejects unauthenticated dashboard websocket requests", async () => {
    const response = middleware(new NextRequest("http://localhost/ws/dashboard"));

    expect(response.status).toBe(401);
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "unauthorized",
        message: "Sign in required.",
      },
    });
  });

  it("returns 503 for dashboard websocket requests when the hub base URL is missing", async () => {
    delete process.env.AHAND_HUB_BASE_URL;

    const request = new NextRequest("http://localhost/ws/dashboard", {
      headers: { cookie: "ahand_hub_session=session-token" },
    });
    const response = middleware(request);

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "hub_unavailable",
        message: "Unable to reach the hub right now.",
      },
    });
  });

  it("does not set cookies when the upstream login token is an empty string", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ token: "" }), {
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
  });

  it("handles non-JSON upstream responses without crashing", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("<html>Bad Gateway</html>", {
          status: 502,
          headers: { "content-type": "text/html" },
        }),
      ),
    );

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.status).toBe(502);
    expect(response.cookies.get("ahand_hub_session")).toBeUndefined();
  });

  it("returns 503 when the login hub base URL is missing", async () => {
    delete process.env.AHAND_HUB_BASE_URL;
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const request = new NextRequest("http://localhost/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password: "shared-secret" }),
    });

    const response = await loginPost(request);

    expect(response.status).toBe(503);
    await expect(response.json()).resolves.toEqual({
      error: {
        code: "hub_unavailable",
        message: "Unable to reach the hub right now.",
      },
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
