import path from "node:path";
import type { NextConfig } from "next";

function websocketRewriteDestination() {
  const hubBaseUrl = process.env.AHAND_HUB_BASE_URL;
  if (!hubBaseUrl) {
    return null;
  }

  return new URL("/ws/dashboard", hubBaseUrl.replace(/\/$/, "")).toString();
}

const nextConfig: NextConfig = {
  reactStrictMode: true,
  async rewrites() {
    const destination = websocketRewriteDestination();
    if (!destination) {
      return [];
    }

    return [
      {
        source: "/ws/dashboard",
        destination,
      },
    ];
  },
  turbopack: {
    root: path.join(__dirname, "../.."),
  },
};

export default nextConfig;
