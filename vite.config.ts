import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

const host = process.env.TAURI_DEV_HOST;

// Tauri expects a fixed dev port and must not watch the Rust side or the private docs.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**", "**/tools/**", "**/internal/**", "**/target/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    // WebView2 on Windows is current Chromium; WKWebView on macOS 14+ is Safari 17.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome120" : "safari17",
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
