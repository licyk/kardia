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
    },
  },
  test: {
    environment: "node",
    include: ["packages/*/tests/**/*.spec.ts"],
  },
});
