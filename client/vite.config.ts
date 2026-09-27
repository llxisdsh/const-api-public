import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: 15420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
    warmup: {
      clientFiles: ["./src/main.tsx"],
    },
  },
  optimizeDeps: {
    entries: ["./index.html"],
    include: [
      "@tauri-apps/api/core",
      "@tauri-apps/api/window",
      "lucide-react",
      "react",
      "react-dom/client",
      "simple-icons",
    ],
  },
});
