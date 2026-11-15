import { execFileSync, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { checkUpstream, upstream, UPSTREAM_SHA } from "../tests/server-parity/upstream.ts";

if (!existsSync(upstream)) {
  run("git", ["init", upstream], dirname(upstream));
  run("git", ["fetch", "--depth=1", "https://github.com/cordiverse/cordis.git", UPSTREAM_SHA]);
  run("git", ["checkout", "--detach", "FETCH_HEAD"]);
}
checkUpstream();
const types = join(upstream, ".file-type-reference");
const typeSha = "7fc6d9ff7cc01987aa73006757792a5ca198023a";
if (!existsSync(types)) {
  run("git", ["init", types], upstream);
  run("git", ["fetch", "--depth=1", "https://github.com/sindresorhus/file-type.git", typeSha], types);
  run("git", ["checkout", "--detach", "FETCH_HEAD"], types);
}
if (execFileSync("git", ["rev-parse", "HEAD"], { cwd: types, encoding: "utf8" }).trim() !== typeSha) {
  throw new Error("file-type fixture checkout differs from the fixed 20.5.0 baseline");
}
run("corepack", ["yarn", "install", "--immutable"]);
run("corepack", ["yarn", "build", "core"]);
run("corepack", ["yarn", "build"]);
run("corepack", ["yarn", "test", "server", "server-acl", "server-proxy", "server-static", "fs"]);

function run(command: string, args: string[], cwd = upstream) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit", shell: process.platform === "win32" && command === "corepack" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.status})`);
}
