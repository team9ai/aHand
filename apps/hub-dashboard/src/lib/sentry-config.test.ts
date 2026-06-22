import { describe, expect, it } from "vitest";
import { getBrowserSentryOptions, getServerSentryOptions } from "./sentry-config";

describe("dashboard Sentry config", () => {
  it("disables browser Sentry without a public DSN", () => {
    expect(getBrowserSentryOptions({})).toBeNull();
  });

  it("builds browser options from public env", () => {
    expect(
      getBrowserSentryOptions({
        NEXT_PUBLIC_SENTRY_DSN: "https://public@example.invalid/2",
        SENTRY_ENVIRONMENT: "production",
        SENTRY_RELEASE: "abc123",
      }),
    ).toMatchObject({
      dsn: "https://public@example.invalid/2",
      environment: "production",
      release: "abc123",
      sendDefaultPii: false,
      tracesSampleRate: 0,
    });
  });

  it("prefers server DSN and falls back to public DSN for server runtime", () => {
    expect(
      getServerSentryOptions({
        SENTRY_DSN: "https://server@example.invalid/2",
        NEXT_PUBLIC_SENTRY_DSN: "https://public@example.invalid/2",
      })?.dsn,
    ).toBe("https://server@example.invalid/2");

    expect(
      getServerSentryOptions({
        NEXT_PUBLIC_SENTRY_DSN: "https://public@example.invalid/2",
      })?.dsn,
    ).toBe("https://public@example.invalid/2");
  });
});
