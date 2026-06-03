import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

export default defineConfig({
  // Mount-prefix-relative asset URLs. By default vite hardcodes
  // every CSS `url(...)` and HTML reference to `/assets/...`, which
  // assumes the app is hosted at the URL root. The playground gets
  // mounted at `/api/playground/` (or wherever the user puts it via
  // `PlaygroundPlugin::at(...)`), so those root-relative paths
  // 404 on every font reference. `base: "./"` makes vite emit
  // relative URLs (`./inter-X.woff2` from inside a CSS file at
  // `/api/playground/assets/index-Y.css`), which the browser
  // correctly resolves against the loader's URL — no matter what
  // path the plugin is mounted under.
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
