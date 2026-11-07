import { Context } from "@kardiajs/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  Bridge,
  BridgeError,
  type BridgeStatus,
  type ClientRequest,
  type HostMessage,
  type OpenReply,
  type OpenRequest,
  RemoteService,
  type ServiceInfo,
  type Transport,
} from "../src/index.ts";

/** A transport whose replies the test scripts, standing in for the host at the transport boundary. */
class ScriptedTransport implements Transport {
  readonly opens: { request: OpenRequest; onMessage: (message: HostMessage) => void; resolve: (reply: OpenReply) => void; reject: (reason: unknown) => void }[] = [];
  readonly requests: ClientRequest[] = [];
  reply: (request: ClientRequest) => unknown = () => null;

  open(request: OpenRequest, onMessage: (message: HostMessage) => void) {
    return new Promise<OpenReply>((resolve, reject) => this.opens.push({ request, onMessage, resolve, reject }));
  }

  async request(request: ClientRequest) {
    this.requests.push(request);
    return this.reply(request);
  }

  /** Answers the latest `open`. */
  accept(session: number, services: ServiceInfo[] = []) {
    this.opens.at(-1)!.resolve({ session, protocol: 1, services });
  }

  push(message: HostMessage) {
    this.opens.at(-1)!.onMessage(message);
  }

  sent(type: ClientRequest["type"]) {
    return this.requests.filter((request) => request.type === type);
  }
}

const tick = () => vi.advanceTimersByTimeAsync(0);

async function setup(config: { timeout?: number } = {}) {
  const root = new Context();
  root._root.logger.exporters.set(-1, { export: () => {} });
  const transport = new ScriptedTransport();
  const fiber = root.plugin(Bridge, { transport, client: "main", retryDelay: 100, ...config });
  await tick();
  return { root, transport, fiber };
}

describe("Bridge client", () => {
  beforeEach(() => void vi.useFakeTimers());
  afterEach(() => void vi.useRealTimers());

  it("opens a session with the protocol version and its label", async () => {
    const { transport } = await setup();
    expect(transport.opens.map((open) => open.request)).toEqual([{ type: "open", protocol: 1, client: "main" }]);
  });

  it("mirrors host services, so plugins depending on them run while they are exposed", async () => {
    const { root, transport } = await setup();
    const values: unknown[] = [];
    const consumer = root.inject(["host.echo"], (ctx) => void values.push((ctx.get("host.echo") as RemoteService).value));
    transport.accept(1, [{ name: "echo", id: 1, value: { v: 1 } }]);
    await tick();
    expect(consumer.state).toBe("active");

    // a new exposure of the same name restarts the dependent
    transport.push({ type: "services", services: [{ name: "echo", id: 2, value: { v: 2 } }] });
    await tick();
    expect(values).toEqual([{ v: 1 }, { v: 2 }]);

    transport.push({ type: "services", services: [] });
    await tick();
    expect(consumer.state).toBe("pending");
  });

  it("applies the open reply before messages pushed earlier", async () => {
    const { root, transport } = await setup();
    transport.push({ type: "services", services: [{ name: "late", id: 2, value: null }] });
    transport.accept(1, [{ name: "early", id: 1, value: null }]);
    await tick();
    expect(root.live("host.late")).toBeInstanceOf(RemoteService);
    expect(root.live("host.early")).toBeUndefined();
  });

  it("calls host methods through its session", async () => {
    const { root, transport } = await setup();
    transport.reply = (request) => (request.type === "call" ? { echoed: request.args } : null);
    transport.accept(7, [{ name: "echo", id: 1, value: null }]);
    await tick();
    const echo = root.get("host.echo") as RemoteService<{ echo(args: number): { echoed: number } }>;
    await expect(echo.call("echo", 3)).resolves.toEqual({ echoed: 3 });
    expect(transport.sent("call")).toEqual([{ type: "call", session: 7, service: "echo", method: "echo", args: 3 }]);
  });

  it("normalizes failures: host errors keep their type, anything else is failed", async () => {
    const { root, transport } = await setup();
    transport.accept(1, [{ name: "echo", id: 1, value: null }]);
    await tick();
    const bridge = root.get("bridge");
    transport.reply = () => Promise.reject({ type: "unavailable", message: "gone" });
    await expect(bridge.call("echo", "x")).rejects.toEqual(new BridgeError("unavailable", "gone"));
    transport.reply = () => Promise.reject("command kardia_bridge not found");
    await expect(bridge.call("echo", "x")).rejects.toEqual(new BridgeError("failed", "command kardia_bridge not found"));
  });

  it("times out requests the host does not answer", async () => {
    const { root, transport } = await setup({ timeout: 50 });
    transport.accept(1);
    await tick();
    transport.reply = () => new Promise(() => {});
    const call = root.get("bridge").call("echo", "x");
    const assertion = expect(call).rejects.toMatchObject({ type: "timeout" });
    await vi.advanceTimersByTimeAsync(50);
    await assertion;
  });

  it("is unavailable while disconnected", async () => {
    const { root } = await setup();
    await expect(root.get("bridge").call("echo", "x")).rejects.toMatchObject({ type: "unavailable" });
  });

  it("re-emits host events under its prefix", async () => {
    const { root, transport } = await setup();
    const events: unknown[] = [];
    root.on("host/agent/status", (payload: unknown) => void events.push(payload));
    transport.accept(1);
    await tick();
    transport.push({ type: "event", name: "agent/status", payload: { busy: true } });
    expect(events).toEqual([{ busy: true }]);
  });

  it("answers host requests with the services it provides", async () => {
    const { root, transport } = await setup();
    transport.accept(3);
    await tick();
    const provider = root.inject(["bridge"], (ctx) => {
      ctx.get("bridge").provide("client.prompt", {
        value: { kind: "text" },
        methods: {
          ask: async (args: { question: string }) => `answer to ${args.question}`,
          fail: () => {
            throw new Error("cancelled");
          },
        },
      });
    });
    await tick();
    expect(transport.sent("provide")).toEqual([{ type: "provide", session: 3, service: "client.prompt", value: { kind: "text" } }]);

    transport.push({ type: "request", id: 1, service: "client.prompt", method: "ask", args: { question: "q" } });
    transport.push({ type: "request", id: 2, service: "client.prompt", method: "fail", args: null });
    transport.push({ type: "request", id: 3, service: "client.prompt", method: "nope", args: null });
    transport.push({ type: "request", id: 4, service: "client.other", method: "ask", args: null });
    await tick();
    expect(transport.sent("resolve")).toEqual([{ type: "resolve", session: 3, id: 1, value: "answer to q" }]);
    expect(transport.sent("reject").map((r) => r.type === "reject" && [r.id, r.error.type])).toEqual([
      [2, "failed"],
      [3, "invalid"],
      [4, "unavailable"],
    ]);

    await provider.dispose();
    expect(transport.sent("withdraw")).toEqual([{ type: "withdraw", session: 3, service: "client.prompt" }]);
  });

  it("reconnects after the host's bridge stops, and provides its services again", async () => {
    const { root, transport } = await setup();
    const statuses: BridgeStatus[] = [];
    root.on("bridge/status", (status) => void statuses.push(status));
    transport.accept(1, [{ name: "echo", id: 1, value: null }]);
    await tick();
    root.inject(["bridge"], (ctx) => void ctx.get("bridge").provide("client.prompt"));
    await tick();

    transport.push({ type: "closed", reason: "stopped" });
    await tick();
    expect(root.live("host.echo")).toBeUndefined();
    expect(transport.opens).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(100);
    expect(transport.opens).toHaveLength(2);
    transport.accept(2, [{ name: "echo", id: 5, value: null }]);
    await tick();
    expect(root.live("host.echo")).toBeInstanceOf(RemoteService);
    expect(transport.sent("provide").map((r) => r.type === "provide" && r.session)).toEqual([1, 2]);
    expect(statuses).toEqual(["connected", "disconnected", "connecting", "connected"]);
  });

  it("retries a failed open", async () => {
    const { transport } = await setup();
    transport.opens[0].reject(new Error("the bridge is not running"));
    await tick();
    await vi.advanceTimersByTimeAsync(100);
    expect(transport.opens).toHaveLength(2);
  });

  it("does not reconnect when another client with its label replaced it", async () => {
    const { root, transport } = await setup();
    transport.accept(1);
    await tick();
    transport.push({ type: "closed", reason: "replaced" });
    await vi.advanceTimersByTimeAsync(1000);
    expect(transport.opens).toHaveLength(1);
    expect(root.get("bridge").status).toBe("replaced");
  });

  it("closes its session and withdraws host services when it stops", async () => {
    const { root, transport, fiber } = await setup();
    transport.accept(4, [{ name: "echo", id: 1, value: null }]);
    await tick();
    await fiber.dispose();
    expect(transport.sent("close")).toEqual([{ type: "close", session: 4 }]);
    expect(root.live("host.echo")).toBeUndefined();
  });

  it("closes a session whose open answers after it stopped", async () => {
    const { transport, fiber } = await setup();
    await fiber.dispose();
    transport.accept(9);
    await tick();
    expect(transport.sent("close")).toEqual([{ type: "close", session: 9 }]);
  });
});
