export function readWsToken(): string | null {
  if (typeof document === "undefined") {
    return null;
  }

  const entry = document.cookie
    .split("; ")
    .find((part) => part.startsWith("ahand_hub_ws_token="));

  return entry ? decodeURIComponent(entry.split("=")[1]) : null;
}
