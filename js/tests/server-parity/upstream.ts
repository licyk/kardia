import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

export const UPSTREAM_SHA = "36dc7fb5e271143039e6b8b6df59840fc0886c1b";
export const upstream = fileURLToPath(new URL("../../../cordis/", import.meta.url));

export function checkUpstream() {
  const sha = execFileSync("git", ["rev-parse", "HEAD"], { cwd: upstream, encoding: "utf8" }).trim();
  if (sha !== UPSTREAM_SHA) throw new Error(`Expected Cordis ${UPSTREAM_SHA}, found ${sha}`);
}
