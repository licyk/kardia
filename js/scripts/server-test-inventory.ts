import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { checkUpstream, upstream, UPSTREAM_SHA } from "../tests/server-parity/upstream.ts";

checkUpstream();
const tests: { source: string; name: string; rustName: string }[] = [];
for (const component of ["core", "acl", "proxy", "static"]) {
  const source = `packages/server/${component}/tests/index.spec.ts`;
  const code = readFileSync(resolve(upstream, source), "utf8");
  // The pinned suites declare every case as a single-line, single-quoted it().
  for (const match of code.matchAll(/^\s*it\('((?:\\.|[^'])*)'/gm)) {
    const name = match[1].replace(/\\'/g, "'");
    tests.push({ source, name, rustName: name.replace(/[^a-zA-Z0-9]+/g, "_").replace(/^_|_$/g, "").toLowerCase() });
  }
}
const output = JSON.stringify({ upstream: UPSTREAM_SHA, tests }, null, 2) + "\n";
const path = fileURLToPath(new URL("../tests/server-parity/inventory.json", import.meta.url));
if (process.argv.includes("--check")) {
  if (readFileSync(path, "utf8").replace(/\r\n/g, "\n") !== output) throw new Error("Server test inventory is stale; run bun run server:inventory");
} else {
  writeFileSync(path, output);
}
console.log(`${tests.length} upstream test declarations inventoried`);
