import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
  plugins: [solidPlugin()],
  server: {
    port: 5173,
    proxy: {
      "/api": "http://localhost:3000",
      "/ws": { target: "ws://localhost:3000", ws: true },
      "/dashboard/ws": { target: "ws://localhost:3000", ws: true },
      "/devices": "http://localhost:3000",
    },
  },
  build: {
    target: "esnext",
  },
});
