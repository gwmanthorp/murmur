import { defineConfig } from "vite";
import { resolve } from "path";

// Tauri expects a fixed dev server port.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "chrome120",
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        overlay: resolve(__dirname, "overlay.html"),
        onboarding: resolve(__dirname, "onboarding.html"),
      },
    },
  },
});
