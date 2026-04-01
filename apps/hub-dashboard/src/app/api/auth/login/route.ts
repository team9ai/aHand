import { NextRequest, NextResponse } from "next/server";

export async function POST(request: NextRequest) {
  let body: unknown;

  try {
    body = await request.json();
  } catch {
    return NextResponse.json({ error: "invalid_json" }, { status: 400 });
  }

  const baseUrl = process.env.AHAND_HUB_BASE_URL;
  if (!baseUrl) {
    return NextResponse.json({ error: "hub_unavailable" }, { status: 503 });
  }

  let response: Response;
  try {
    response = await fetch(`${baseUrl}/api/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    return NextResponse.json({ error: "hub_unavailable" }, { status: 503 });
  }

  const payload = await response.json().catch(() => ({}));
  const next = NextResponse.json(payload, { status: response.status });

  if (response.ok && typeof payload?.token === "string" && payload.token.length > 0) {
    next.cookies.set("ahand_hub_session", payload.token, {
      httpOnly: true,
      path: "/",
      sameSite: "lax",
      secure: process.env.NODE_ENV === "production",
    });
  }

  return next;
}
