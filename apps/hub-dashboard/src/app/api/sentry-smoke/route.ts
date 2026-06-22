import { NextRequest } from "next/server";

export async function POST(request: NextRequest) {
  const expected = process.env.AHAND_HUB_DASHBOARD_SENTRY_SMOKE_TOKEN?.trim();
  if (!expected) {
    return new Response("Not found", { status: 404 });
  }

  const actual = request.headers.get("x-ahand-sentry-smoke-token");
  if (actual !== expected) {
    return new Response("Forbidden", { status: 403 });
  }

  throw new Error("aHand dashboard Sentry smoke test");
}
