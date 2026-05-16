import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [solid(), tailwindcss()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    proxy: {
      "/healthz": "http://localhost:3000",
      "/version": "http://localhost:3000",
      "/ws/flows": {
        target: "ws://localhost:3000",
        ws: true,
        rewriteWsOrigin: true,
      },
    },
  },
});
