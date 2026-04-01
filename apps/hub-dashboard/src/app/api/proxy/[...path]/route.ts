import { NextRequest, NextResponse } from "next/server";

export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ path: string[] }> },
) {
  const { path } = await params;
  const session = request.cookies.get("ahand_hub_session")?.value ?? "";
  const upstream = new URL(
    path.join("/"),
    `${process.env.AHAND_HUB_BASE_URL?.replace(/\/?$/, "/")}`,
  );
  upstream.search = request.nextUrl.search;

  const response = await fetch(upstream.toString(), {
    headers: {
      authorization: `Bearer ${session}`,
      accept: request.headers.get("accept") ?? "application/json",
    },
    cache: "no-store",
  });

  return new NextResponse(response.body, {
    status: response.status,
    headers: response.headers,
  });
}
