import { NextResponse } from "next/server";

export async function POST() {
  const response = NextResponse.redirect(new URL("/login", "http://localhost"), 303);

  response.cookies.delete("ahand_hub_session");
  response.cookies.delete("ahand_hub_ws_token");
  response.headers.set("location", "/login");

  return response;
}
