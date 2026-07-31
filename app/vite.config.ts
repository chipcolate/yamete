import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Watching src-tauri makes the dev server reload on every Rust rebuild, which
      // fights with Tauri's own watcher and can stop it starting at all.
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "safari15",
    minify: "esbuild",
    sourcemap: false,
  },
});
