import { defineConfig } from "vite";

// Vite config for the StrataGraph desktop UI (vanilla TS, no framework).
//
// Tauri expects a fixed dev port and the build output where `frontendDist`
// points (`../ui/dist`, i.e. the default `dist/` here). `clearScreen: false`
// keeps Rust compiler output visible during `tauri dev`.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
  },
});
