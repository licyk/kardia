// The client against the real Rust host: `crates/kardia-bridge/examples/stdio_host.rs`,
// spoken to over its stdin and stdout.

import { type ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { Context } from "@kardiajs/core";
import { afterEach, describe, expect, it } from "vitest";

import { Bridge, type BridgeStatus, type ClientRequest, type HostMessage, type OpenReply, type OpenRequest, RemoteService, type Transport } from "../src/index.ts";

const HOST = fileURLToPath(
  new URL(`../../../../target/debug/examples/stdio_host${process.platform === "win32" ? ".exe" : ""}`, import.meta.url),
);

/** A transport over the host process's stdin and stdout. */
class StdioTransport implements Transport {
  private readonly process: ChildProcessWithoutNullStreams = spawn(HOST, { stdio: ["pipe", "pipe", "pipe"] });
  private readonly pending = new Map<number, { resolve: (value: unknown) => void; reject: (reason: unknown) => void }>();
  private next = 0;
  private onMessage: ((message: HostMessage) => void) | undefined;
  readonly stderr: string[] = [];

  constructor() {
    createInterface({ input: this.process.stdout }).on("line", (line) => {
      const output = JSON.parse(line);
      if ("message" in output) return this.onMessage?.(output.message);
      const pending = this.pending.get(output.id)!;
      this.pending.delete(output.id);
      if ("error" in output) pending.reject(output.error);
      else pending.resolve(output.ok);
    });
    createInterface({ input: this.process.stderr }).on("line", (line) => this.stderr.push(line));
  }

  open(request: OpenRequest, onMessage: (message: HostMessage) => void) {
    this.onMessage = onMessage;
    return this.send(request) as Promise<OpenReply>;
  }

  request(request: ClientRequest) {
    return this.send(request);
  }

  private send(request: unknown) {
    const id = ++this.next;
    return new Promise<unknown>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.process.stdin.write(`${JSON.stringify({ id, request })}\n`);
    });
  }

  kill() {
    this.process.kill();
  }
}

/** Retries `attempt` until it resolves (the host may not have seen an earlier request yet). */
async function eventually<T>(attempt: () => Promise<T>, what: string): Promise<T> {
  const deadline = Date.now() + 10_000;
  while (true) {
    try {
      return await attempt();
    } catch (error) {
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}: ${JSON.stringify(error)}`);
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  }
}

/** Resolves once `predicate` holds, polling. */
async function until(predicate: () => boolean, what: string) {
  const deadline = Date.now() + 10_000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

let transport: StdioTransport | undefined;

afterEach(() => transport?.kill());

async function connect() {
  transport = new StdioTransport();
  const root = new Context();
  const statuses: BridgeStatus[] = [];
  root.on("bridge/status", (status) => void statuses.push(status));
  root.plugin(Bridge, { transport, retryDelay: 50, timeout: 5_000 });
  // the host mounts its plugins after the bridge, so services arrive in pushes
  await until(() => !!root.live("host.echo") && !!root.live("host.control") && !!root.live("host.info"), "the host services");
  return { root, statuses, bridge: root.get("bridge") };
}

describe("Bridge against the Rust host", () => {
  it("mirrors host services with their values and calls them", async () => {
    const { root } = await connect();
    expect((root.get("host.info") as RemoteService).value).toEqual({ protocol: 1 });
    const echo = root.get("host.echo") as RemoteService;
    await expect(echo.call("echo", { a: [1, 2] })).resolves.toEqual({ a: [1, 2] });
    await expect(echo.call("fail", "nope")).rejects.toMatchObject({ type: "failed", message: "nope" });
    await expect(echo.call("missing")).rejects.toMatchObject({ type: "invalid" });
  });

  it("runs client plugins that depend on host services", async () => {
    const { root } = await connect();
    const results: unknown[] = [];
    const fiber = root.inject(["host.echo"], async (ctx) => {
      results.push(await (ctx.get("host.echo") as RemoteService).call("echo", "from a plugin"));
    });
    await until(() => results.length === 1, "the dependent plugin");
    expect(fiber.state).toBe("active");
    expect(results).toEqual(["from a plugin"]);
  });

  it("receives host events", async () => {
    const { root } = await connect();
    const events: unknown[] = [];
    root.on("host/agent/status", (payload: unknown) => void events.push(payload));
    await (root.get("host.control") as RemoteService).call("emit", { name: "agent/status", payload: { busy: true } });
    await until(() => events.length === 1, "the event");
    expect(events).toEqual([{ busy: true }]);
  });

  it("serves host calls into a service it provides", async () => {
    const { root } = await connect();
    const control = root.get("host.control") as RemoteService;
    await expect(control.call("askClient", { method: "ask", args: "q" })).rejects.toMatchObject({ type: "unavailable" });

    root.inject(["bridge"], (ctx) => {
      ctx.get("bridge").provide("client.prompt", {
        methods: {
          ask: (question: string) => `answer to ${question}`,
          refuse: () => {
            throw new Error("refused");
          },
        },
      });
    });
    await expect(eventually(() => control.call("askClient", { method: "ask", args: "q" }), "the provision")).resolves.toBe(
      "answer to q",
    );
    await expect(control.call("askClient", { method: "refuse" })).rejects.toMatchObject({ type: "failed", message: "refused" });
  });

  it("reconnects after the host's bridge restarts, and provides its services again", async () => {
    const { root, statuses } = await connect();
    root.inject(["bridge"], (ctx) => {
      ctx.get("bridge").provide("client.prompt", { methods: { ask: () => "still here" } });
    });
    const dependent = root.inject(["host.echo"], () => {});
    await until(() => dependent.state === "active", "the dependent plugin");

    await (root.get("host.control") as RemoteService).call("restartBridge");
    await until(() => statuses.includes("disconnected"), "the disconnection");
    await until(() => statuses.at(-1) === "connected" && !!root.live("host.control"), "the reconnection");
    await until(() => dependent.state === "active", "the dependent plugin to come back");
    const control = root.get("host.control") as RemoteService;
    await expect(eventually(() => control.call("askClient", { method: "ask" }), "the provision")).resolves.toBe("still here");
  });
});
