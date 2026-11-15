import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const UPSTREAM_SHA = "f6290dbbf1558cb3a870a58b1f97bacc0d3a9960";
export const upstream = fileURLToPath(new URL("../../../cordis/", import.meta.url));

export function checkUpstream() {
  const sha = execFileSync("git", ["rev-parse", "HEAD"], { cwd: upstream, encoding: "utf8" }).trim();
  if (sha !== UPSTREAM_SHA) throw new Error(`Expected Cordis ${UPSTREAM_SHA}, found ${sha}`);
}
