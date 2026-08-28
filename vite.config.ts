import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri drives this dev server; the port is fixed and must match
// `build.devUrl` in src-tauri/tauri.conf.json.
const DEV_PORT = 1420;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],

  // Tauri expects a fixed port and shows Rust errors we do not want cleared.
  clearScreen: false,

  server: {
    port: DEV_PORT,
    strictPort: true,
    watch: {
      // The Rust side has its own watcher; never let Vite walk target/.
      ignored: ["**/src-tauri/**"],
    },
  },

  // Only TAURI_ENV_* and VITE_* reach the client. No secrets are ever
  // exposed to the WebView (AGENTS.md).
  envPrefix: ["VITE_", "TAURI_ENV_"],

  build: {
    // WebView2 on Windows and WKWebView on macOS both handle modern output.
    target: "es2022",
    minify: process.env.TAURI_ENV_DEBUG ? false : "esbuild",
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
