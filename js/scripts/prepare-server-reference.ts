import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname } from "node:path";
import { checkUpstream, upstream, UPSTREAM_SHA } from "../tests/server-parity/upstream.ts";

if (!existsSync(upstream)) {
  run("git", ["init", upstream], dirname(upstream));
  run("git", ["fetch", "--depth=1", "https://github.com/cordiverse/cordis.git", UPSTREAM_SHA]);
  run("git", ["checkout", "--detach", "FETCH_HEAD"]);
}
checkUpstream();
run("corepack", ["yarn", "install", "--immutable"]);
run("corepack", ["yarn", "build", "core"]);
run("corepack", ["yarn", "build"]);
run("corepack", ["yarn", "test", "server", "server-acl", "server-proxy", "server-static"]);

function run(command: string, args: string[], cwd = upstream) {
  const result = spawnSync(command, args, { cwd, stdio: "inherit", shell: process.platform === "win32" && command === "corepack" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.status})`);
}
