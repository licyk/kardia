import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["tests/server-parity/**/*.spec.ts"],
    testTimeout: 10_000,
    hookTimeout: 15_000,
  },
});
