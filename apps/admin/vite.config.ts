import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
  plugins: [solidPlugin()],
  server: {
    port: 5174,
    proxy: {
      "/api": "http://localhost:9800",
    },
  },
  build: {
    target: "esnext",
  },
});
