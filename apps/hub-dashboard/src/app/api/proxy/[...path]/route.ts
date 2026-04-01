import { NextRequest, NextResponse } from "next/server";

export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ path: string[] }> },
) {
  const { path } = await params;
  const session = request.cookies.get("ahand_hub_session")?.value ?? "";
  const baseUrl = process.env.AHAND_HUB_BASE_URL;

  if (!session) {
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  }

  if (!baseUrl) {
    return NextResponse.json({ error: "hub_unavailable" }, { status: 503 });
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
    return NextResponse.json({ error: "hub_unavailable" }, { status: 503 });
  }

  return new NextResponse(response.body, {
    status: response.status,
    headers: response.headers,
  });
}
