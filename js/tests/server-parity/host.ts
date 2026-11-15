import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { checkUpstream, upstream } from "./upstream.ts";

export async function startHost(scenario: string, implementation: "upstream" | "rust" = "upstream", env: Record<string,string> = {}) {
  if (implementation === "upstream") checkUpstream();
  const binary = fileURLToPath(new URL(`../../../target/debug/examples/server-reference${process.platform === "win32" ? ".exe" : ""}`, import.meta.url));
  const child = implementation === "upstream"
    ? spawn("node", ["--expose-internals", fileURLToPath(new URL("./upstream-host.mjs", import.meta.url)), upstream, scenario], { stdio: ["pipe", "pipe", "pipe"], env: {...process.env,...env} })
    : spawn(binary, [scenario], { stdio: ["pipe", "pipe", "pipe"], env: {...process.env,...env} });
  let stderr = "";
  child.stderr.on("data", (data: Buffer) => { stderr += data.toString(); });
  const exited = new Promise<void>((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`Host exited ${code}: ${stderr}`)));
  });
  // Observe failures even if startup fails before returning a host to the test.
  void exited.catch(() => {});
  const lines = createInterface({ input: child.stdout });
  const ready = new Promise<{ url: string }>((resolve, reject) => {
    lines.once("line", (line) => { try { resolve(JSON.parse(line)); } catch (error) { reject(error); } });
    child.once("exit", () => reject(new Error(`Host exited before readiness: ${stderr}`)));
    child.once("error", reject);
  });
  try {
    const { url } = await Promise.race([ready, deadline(10_000)]);
    return { url, async stop() {
      child.stdin.end('{"op":"stop"}\n');
      try { await Promise.race([exited, deadline(5_000)]); }
      finally { child.kill(); lines.close(); }
    } };
  } catch (error) {
    child.kill(); lines.close(); throw error;
  }
}

function deadline(ms: number): Promise<never> {
  return new Promise((_, reject) => {
    const timer = setTimeout(() => reject(new Error(`Host deadline exceeded (${ms} ms)`)), ms);
    timer.unref();
  });
}
