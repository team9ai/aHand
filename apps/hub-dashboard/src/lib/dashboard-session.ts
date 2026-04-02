import { cookies } from "next/headers";

const SESSION_COOKIE = "ahand_hub_session";

export async function verifyDashboardSession(): Promise<string | null> {
  const requestCookies = await cookies();
  const session = requestCookies.get(SESSION_COOKIE)?.value;

  if (!session) {
    return null;
  }

  const baseUrl = process.env.AHAND_HUB_BASE_URL;
  if (!baseUrl) {
    return null;
  }

  try {
    const response = await fetch(`${baseUrl}/api/auth/verify`, {
      headers: {
        authorization: `Bearer ${session}`,
      },
      cache: "no-store",
    });

    if (!response.ok) {
      return null;
    }
  } catch {
    return null;
  }

  return session;
}
