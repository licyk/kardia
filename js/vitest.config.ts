import { fileURLToPath, URL } from "node:url";

import { defineConfig } from "vitest/config";

const source = (name: string) => fileURLToPath(new URL(`./packages/${name}/src/index.ts`, import.meta.url));

export default defineConfig({
  resolve: {
    alias: {
      "@kardiajs/core": source("core"),
      "@kardiajs/timer": source("timer"),
      "@kardiajs/bridge": source("bridge"),
      "@kardiajs/tauri": source("tauri"),
      "@kardiajs/vue": source("vue"),
    },
  },
  // `#/` imports resolve to each package's sources (package.json `imports`, condition `source`);
  // tests run in Vite's server-side environment, which reads its own conditions
  ssr: { resolve: { conditions: ["source"] } },
  test: {
    environment: "node",
    include: ["packages/*/tests/**/*.spec.ts"],
  },
});
