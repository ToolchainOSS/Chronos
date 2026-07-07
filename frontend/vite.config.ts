import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The dev server proxies the WebSocket egress and API to the Rust backend so the
// frontend can be developed against a locally running server without CORS issues.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/ws": {
        target: "ws://localhost:8080",
        ws: true,
      },
      "/healthz": "http://localhost:8080",
      "/readyz": "http://localhost:8080",
      "/metrics": "http://localhost:8080",
    },
  },
});
