import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// The bench backend runs on 127.0.0.1:8113 (see harness/backend/umbral.toml).
// Proxying /api through Vite keeps the dev app same-origin — fetch and
// EventSource both work without CORS ceremony; the backend also allows the
// 5173 origin directly if you bypass the proxy.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8113",
        changeOrigin: true,
        ws: false,
      },
    },
  },
});
