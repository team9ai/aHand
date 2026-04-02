import type { NextRequest } from "next/server";
import { NextResponse } from "next/server";
import { dashboardErrorResponse } from "@/lib/api-error";

export function middleware(request: NextRequest) {
  if (request.nextUrl.pathname === "/ws/dashboard") {
    return rewriteDashboardWebSocket(request);
  }

  const session = request.cookies.get("ahand_hub_session");

  if (!session && request.nextUrl.pathname.startsWith("/api/proxy/")) {
    return dashboardErrorResponse("unauthorized", "Sign in required.", 401);
  }

  if (
    !session &&
    !request.nextUrl.pathname.startsWith("/login") &&
    !request.nextUrl.pathname.startsWith("/api/auth")
  ) {
    return NextResponse.redirect(new URL("/login", request.url));
  }

  return NextResponse.next();
}

function rewriteDashboardWebSocket(request: NextRequest) {
  const session = request.cookies.get("ahand_hub_session");
  if (!session) {
    return dashboardErrorResponse("unauthorized", "Sign in required.", 401);
  }

  const hubBaseUrl = process.env.AHAND_HUB_BASE_URL;
  if (!hubBaseUrl) {
    return dashboardErrorResponse("hub_unavailable", "Unable to reach the hub right now.", 503);
  }

  return NextResponse.rewrite(new URL("/ws/dashboard", hubBaseUrl.replace(/\/$/, "")));
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon.ico).*)"],
};
