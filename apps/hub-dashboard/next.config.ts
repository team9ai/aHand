import path from "node:path";
import { withSentryConfig } from "@sentry/nextjs";
import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  reactStrictMode: true,
  turbopack: {
    root: path.join(__dirname, "../.."),
  },
};

export default withSentryConfig(nextConfig, {
  org: process.env.SENTRY_ORG ?? "sentry",
  project: process.env.SENTRY_PROJECT ?? "ahand-dashboard",
  silent: !process.env.CI,
});
