import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const UPSTREAM_SHA = "6ad12cf2b39e7a22fe54b2f4568e948ecc778e97";
export const upstream = fileURLToPath(new URL("../../../cordis/", import.meta.url));

export function checkUpstream() {
  const sha = execFileSync("git", ["rev-parse", "HEAD"], { cwd: upstream, encoding: "utf8" }).trim();
  if (sha !== UPSTREAM_SHA) throw new Error(`Expected Cordis ${UPSTREAM_SHA}, found ${sha}`);
}
