import { NextRequest, NextResponse } from "next/server";
import { dashboardErrorResponse } from "@/lib/api-error";

export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ path: string[] }> },
) {
  const { path } = await params;
  const session = request.cookies.get("ahand_hub_session")?.value ?? "";
  const baseUrl = process.env.AHAND_HUB_BASE_URL;

  if (!session) {
    return dashboardErrorResponse("unauthorized", "Sign in required.", 401);
  }

  if (!baseUrl) {
    return dashboardErrorResponse("hub_unavailable", "Unable to reach the hub right now.", 503);
  }

  const upstream = new URL(
    path.join("/"),
    `${baseUrl.replace(/\/?$/, "/")}`,
  );
  upstream.search = request.nextUrl.search;

  let response: Response;
  try {
    const headers: Record<string, string> = {
      authorization: `Bearer ${session}`,
      accept: request.headers.get("accept") ?? "application/json",
    };
    const lastEventId = request.headers.get("last-event-id");
    if (lastEventId) {
      headers["last-event-id"] = lastEventId;
    }

    response = await fetch(upstream.toString(), {
      headers,
      cache: "no-store",
    });
  } catch {
    return dashboardErrorResponse("hub_unavailable", "Unable to reach the hub right now.", 503);
  }

  return new NextResponse(response.body, {
    status: response.status,
    headers: response.headers,
  });
}

export async function POST(
  request: NextRequest,
  { params }: { params: Promise<{ path: string[] }> },
) {
  const { path } = await params;
  const session = request.cookies.get("ahand_hub_session")?.value ?? "";
  const baseUrl = process.env.AHAND_HUB_BASE_URL;

  if (!session) {
    return dashboardErrorResponse("unauthorized", "Sign in required.", 401);
  }

  if (!baseUrl) {
    return dashboardErrorResponse("hub_unavailable", "Unable to reach the hub right now.", 503);
  }

  const upstream = new URL(path.join("/"), `${baseUrl.replace(/\/?$/, "/")}`);
  upstream.search = request.nextUrl.search;

  let response: Response;
  try {
    const bodyBuffer = await request.arrayBuffer();
    const headers: Record<string, string> = {
      authorization: `Bearer ${session}`,
      "content-type": request.headers.get("content-type") ?? "application/octet-stream",
      accept: request.headers.get("accept") ?? "application/octet-stream",
    };
    response = await fetch(upstream.toString(), {
      method: "POST",
      headers,
      body: bodyBuffer,
      cache: "no-store",
    });
  } catch {
    return dashboardErrorResponse("hub_unavailable", "Unable to reach the hub right now.", 503);
  }

  // Allowlist upstream response headers to avoid silently relaying
  // set-cookie or other sensitive headers onto the dashboard origin.
  // GET deliberately keeps full passthrough because SSE resume needs it;
  // POST is a mutation path where a leaked Set-Cookie would be a silent
  // session-injection primitive.
  const forwardedHeaders = new Headers();
  for (const key of ["content-type", "content-length", "cache-control", "etag"]) {
    const value = response.headers.get(key);
    if (value) forwardedHeaders.set(key, value);
  }
  return new NextResponse(response.body, {
    status: response.status,
    headers: forwardedHeaders,
  });
}
