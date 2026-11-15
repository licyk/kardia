// Builds every package into its lib/ in dependency order, after copying the
// licence files npm should ship with it. `--clean` removes what a build adds.

import { spawnSync } from "node:child_process";
import { copyFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const PACKAGES = ["core", "timer", "bridge", "tauri", "vue"];

const workspace = fileURLToPath(new URL("..", import.meta.url));
const at = (path: string) => join(workspace, path);
const clean = process.argv.includes("--clean");

for (const name of PACKAGES) {
  const dir = at(`packages/${name}`);
  rmSync(join(dir, "lib"), { recursive: true, force: true });
  if (clean) {
    rmSync(join(dir, "LICENSE"), { force: true });
    rmSync(join(dir, "NOTICE"), { force: true });
    continue;
  }
  copyFileSync(at("../LICENSE"), join(dir, "LICENSE"));
  copyFileSync(at("NOTICE"), join(dir, "NOTICE"));
  const result = spawnSync(process.execPath, [at("node_modules/typescript/bin/tsc"), "-p", dir], { stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
  console.log(`built @kardiajs/${name}`);
}
