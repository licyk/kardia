import { defineConfig } from "vitest/config";

import base from "./vitest.config.ts";

// End-to-end tests against the Rust host (`cargo build -p kardia-bridge --example stdio_host` first).
export default defineConfig({
  ...base,
  test: {
    ...base.test,
    include: ["packages/*/tests/**/*.e2e.ts"],
    testTimeout: 20_000,
  },
});
