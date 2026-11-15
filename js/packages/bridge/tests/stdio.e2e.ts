// The client against the real Rust host: `crates/kardia-bridge/examples/stdio_host.rs`,
// spoken to over its stdin and stdout.

import { type ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { Context } from "@kardiajs/core";
import { Timer } from "@kardiajs/timer";
import { afterEach, describe, expect, it } from "vitest";

import { Bridge, type BridgeStatus, type ClientRequest, type HostMessage, type OpenReply, type OpenRequest, RemoteService, type Transport } from "#/index";

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

  async close() {
    try {
      // Bridge disposal sends asynchronous withdrawals and a final close request.
      await until(() => this.pending.size === 0, "pending bridge requests to drain");
      const closed = once(this.process, "close");
      this.process.stdin.end();
      await closed;
    } finally {
      this.process.kill();
    }
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
let client: Context | undefined;

afterEach(async () => {
  await client?.fiber.dispose();
  await transport?.close();
});

async function connect() {
  transport = new StdioTransport();
  const root = new Context();
  client = root;
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

interface RecoveryStats {
  loads: number;
  live: number;
  cleaned: number;
  decisions: number;
  overlap: number;
}

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

describe("plugin recovery across the real Rust/JS bridge", () => {
  it.each(["manual", "retry", "host"])("%s preserves unrelated Rust and JS services through three failures", async (mode) => {
    const { root, statuses } = await connect();
    await root.plugin(Timer).settled();
    await until(() => !!root.live("host.recovery"), "the recovery controller");
    const manager = root.get("host.recovery") as RemoteService;
    const control = root.get("host.control") as RemoteService;
    const echo = root.get("host.echo") as RemoteService;
    let ticks = 0;
    let answered = 0;
    const healthy = root.inject(["bridge", "timer"], (ctx) => {
      ctx.get("bridge").provide("client.prompt", { methods: { ask: () => ++answered } });
      ctx.get("timer").interval(2, () => ticks++);
    });
    await healthy.settled();
    await eventually(() => control.call("askClient", { method: "ask" }), "healthy JS service");
    await manager.call("start", mode);
    let starts = 0;
    let stops = 0;
    let live = 0;
    let overlap = false;
    const generations: unknown[] = [];
    const consumer = root.inject(["host.flaky"], async (ctx) => {
      if (live !== 0) overlap = true;
      live++;
      starts++;
      ctx.onDispose(async () => { await sleep(10); live--; stops++; });
      generations.push(await (ctx.get("host.flaky") as RemoteService).call("echo", "consumer"));
    });
    await until(() => consumer.state === "active", "the dependent consumer");
    const connectedStatuses = [...statuses];
    let running = true;
    let probes = 0;
    let monitorError: unknown;
    const monitor = (async () => {
      while (running) {
        const token = ++probes;
        expect(await echo.call("echo", token)).toBe(token);
        expect(await control.call("askClient", { method: "ask" })).toBeGreaterThan(0);
        await sleep(2);
      }
    })().catch((error) => { monitorError = error; });
    try {
      for (let round = 1; round <= 3; round++) {
        const before = { probes, ticks, answered };
        const failed = await manager.call("fail") as RecoveryStats;
        expect(failed).toMatchObject({ loads: round, live: 0, cleaned: round, overlap: 0 });
        await until(() => consumer.state === "pending" && !root.live("host.flaky"), "withdrawal of failed service");
        expect(live).toBe(0);
        expect(stops).toBe(round);
        if (mode === "manual") {
          await sleep(60);
          expect(await manager.call("status")).toMatchObject({ loads: round, live: 0 });
          await expect(root.get("bridge").call("flaky", "echo")).rejects.toMatchObject({ type: "unavailable" });
          await manager.call("recover");
        }
        await until(() => consumer.state === "active" && starts === round + 1, "recovered consumer");
        expect(await manager.call("status")).toMatchObject({ loads: round + 1, live: 1, cleaned: round, overlap: 0 });
        expect(probes).toBeGreaterThan(before.probes);
        expect(ticks).toBeGreaterThan(before.ticks);
        expect(answered).toBeGreaterThan(before.answered);
        expect(healthy.state).toBe("active");
        expect(root.get("host.echo")).toBe(echo);
        expect(root.get("host.control")).toBe(control);
      }
      expect(generations).toEqual([1, 2, 3, 4].map((generation) => ({ generation, value: "consumer" })));
      expect(overlap).toBe(false);
      expect(await manager.call("status")).toMatchObject({ decisions: mode === "host" ? 3 : 0 });
      expect(statuses).toEqual(connectedStatuses);
      await manager.call("stop");
      await until(() => consumer.state === "pending", "final consumer cleanup");
      expect(live).toBe(0);
      expect(stops).toBe(4);
      expect(await manager.call("status")).toMatchObject({ live: 0, cleaned: 4, loads: 4 });
    } finally {
      running = false;
      await monitor;
    }
    expect(monitorError).toBeUndefined();
  });

  it.each(["retry", "host"])("disposing a failed %s provider cancels recovery across the bridge", async (mode) => {
    const { root } = await connect();
    await until(() => !!root.live("host.recovery"), "recovery controller");
    const manager = root.get("host.recovery") as RemoteService;
    await manager.call("start", mode);
    const consumer = root.inject(["host.flaky"], () => {});
    await until(() => consumer.state === "active", "consumer");
    await manager.call("fail");
    await manager.call("stop");
    await sleep(300);
    expect(await manager.call("status")).toMatchObject({ live: 0, cleaned: 1, loads: 1 });
    expect(consumer.state).toBe("pending");
    expect(root.live("host.flaky")).toBeUndefined();
    await expect((root.get("host.echo") as RemoteService).call("echo", "healthy")).resolves.toBe("healthy");
  });
});
