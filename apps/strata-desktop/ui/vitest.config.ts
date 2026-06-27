import { defineConfig } from "vitest/config";

// Vitest config for the graph-view transform tests.
//
// The transform module (src/graphview/build.ts) is pure — no DOM, no Sigma — so
// the default Node environment is all we need. We scope discovery to the source
// tree's `*.test.ts` files so node_modules / dist are never scanned.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
