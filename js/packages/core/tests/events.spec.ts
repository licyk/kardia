import { describe, expect, it, vi } from "vitest";

import { Context } from "#/index";
import { captureLogs, listenerCount, sleep } from "./helpers.ts";

declare module "#/index" {
  interface Events {
    "test/event"(value: number): unknown;
  }
}

describe("Events", () => {
  it("call listeners in registration order, prepended ones first", () => {
    const root = new Context();
    const seq: string[] = [];
    root.on("test/event", () => void seq.push("a"));
    root.on("test/event", () => void seq.push("b"));
    root.on("test/event", () => void seq.push("c"), { prepend: true });
    root.emit("test/event", 1);
    expect(seq).toEqual(["c", "a", "b"]);
  });

  it("remove a listener with its effect or its plugin", async () => {
    const root = new Context();
    const callback = vi.fn();
    const handle = root.on("test/event", callback);
    const fiber = root.plugin((ctx: Context) => void ctx.on("test/event", callback));
    await fiber.settled();
    root.emit("test/event", 1);
    expect(callback).toHaveBeenCalledTimes(2);
    await handle.dispose();
    await fiber.dispose();
    root.emit("test/event", 1);
    expect(callback).toHaveBeenCalledTimes(2);
    expect(listenerCount(root, "test/event")).toBe(0);
  });

  it("once", () => {
    const root = new Context();
    const callback = vi.fn();
    root.once("test/event", callback);
    root.emit("test/event", 1);
    root.emit("test/event", 2);
    expect(callback.mock.calls).toEqual([[1]]);
  });

  it("log a failing listener and run the rest", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    const callback = vi.fn();
    root.on("test/event", () => {
      throw new Error("sync");
    });
    root.on("test/event", async () => {
      throw new Error("async");
    });
    root.on("test/event", callback);
    root.emit("test/event", 1);
    expect(callback).toHaveBeenCalledTimes(1);
    await sleep();
    expect(logs.errors().map((message) => message.text.split("\n")[0])).toEqual(["Error: sync", "Error: async"]);
  });

  it("bail with the first result that is not undefined, null or false", () => {
    const root = new Context();
    root.on("test/event", () => undefined);
    root.on("test/event", () => false);
    root.on("test/event", (value) => value * 2);
    root.on("test/event", () => "never");
    expect(root.bail("test/event", 21)).toBe(42);
  });

  it("serial awaits listeners one at a time", async () => {
    const root = new Context();
    const seq: string[] = [];
    root.on("test/event", async () => {
      await sleep(10);
      seq.push("a");
    });
    root.on("test/event", async () => {
      seq.push("b");
      return "done";
    });
    root.on("test/event", () => void seq.push("c"));
    expect(await root.serial("test/event", 1)).toBe("done");
    expect(seq).toEqual(["a", "b"]);
  });

  it("parallel awaits every listener and aggregates failures", async () => {
    const root = new Context();
    const seq: string[] = [];
    root.on("test/event", async () => {
      await sleep(10);
      seq.push("a");
    });
    root.on("test/event", () => {
      throw new Error("b");
    });
    await expect(root.parallel("test/event", 1)).rejects.toThrow(AggregateError);
    expect(seq).toEqual(["a"]);
  });

  it("deliver internal/service only to listeners in the changed realm", async () => {
    const root = new Context();
    const outer: string[] = [];
    const inner: string[] = [];
    root.on("internal/service", (key) => void outer.push(key));
    const isolated = root.isolate("foo");
    isolated.on("internal/service", (key) => void inner.push(key));
    isolated.provide("foo", 1);
    root.provide("foo", 2);
    root.provide("bar", 3);
    expect(outer).toEqual(["foo", "bar"]);
    expect(inner).toEqual(["foo", "bar"]);
    expect(inner).toHaveLength(2);
  });

  it("report plugin insertion, removal and status changes", async () => {
    const root = new Context();
    const seen: string[] = [];
    root.on("internal/plugin-inserted", (fiber) => void seen.push(`inserted ${fiber.name}`));
    root.on("internal/plugin-removed", (fiber) => void seen.push(`removed ${fiber.name}`));
    root.on("internal/status", (fiber, old, state) => void seen.push(`${fiber.name}: ${old} -> ${state}`));
    const fiber = root.plugin(function foo() {});
    await fiber.settled();
    await fiber.dispose();
    expect(seen).toEqual([
      "inserted foo",
      "foo: pending -> loading",
      "foo: loading -> active",
      "removed foo",
      "foo: active -> unloading",
      "foo: unloading -> disposed",
    ]);
  });
});
