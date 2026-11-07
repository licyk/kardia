// Builds every package into its lib/ in dependency order, after copying the
// licence files npm should ship with it. `--clean` removes what a build adds.

import { spawnSync } from "node:child_process";
import { copyFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";

const PACKAGES = ["core", "timer", "bridge", "tauri", "vue"];

const workspace = fileURLToPath(new URL("..", import.meta.url));
const at = (path: string) => `${workspace}/${path}`;
const clean = process.argv.includes("--clean");

for (const name of PACKAGES) {
  const dir = at(`packages/${name}`);
  rmSync(`${dir}/lib`, { recursive: true, force: true });
  if (clean) {
    rmSync(`${dir}/LICENSE`, { force: true });
    rmSync(`${dir}/NOTICE`, { force: true });
    continue;
  }
  copyFileSync(at("../LICENSE"), `${dir}/LICENSE`);
  copyFileSync(at("NOTICE"), `${dir}/NOTICE`);
  const result = spawnSync(at("node_modules/.bin/tsc"), ["-p", dir], { stdio: "inherit" });
  if (result.status !== 0) process.exit(result.status ?? 1);
  console.log(`built @kardiajs/${name}`);
}
