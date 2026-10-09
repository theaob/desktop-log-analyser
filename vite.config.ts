import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed dev port and handles the rest.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**", "**/target/**", "**/testdata/**"] } },
  build: { target: "es2022", chunkSizeWarningLimit: 1500 },
});
