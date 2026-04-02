import { NextResponse } from "next/server";

export type DashboardErrorCode = "invalid_json" | "unauthorized" | "hub_unavailable";

export function dashboardErrorResponse(
  code: DashboardErrorCode,
  message: string,
  status: number,
) {
  return NextResponse.json(
    {
      error: {
        code,
        message,
      },
    },
    { status },
  );
}
