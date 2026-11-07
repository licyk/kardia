import { describe, expect, it, vi } from "vitest";

import { Context, Level } from "#/index";
import { captureLogs, sleep } from "./helpers.ts";

describe("Isolation", () => {
  it("keeps own provisions inside their isolation", async () => {
    const root = new Context();
    await root
      .plugin(async (ctx: Context) => {
        const isolated = ctx.isolate("foo");
        isolated.provide("foo", {});
        expect(isolated.get("foo")).toBeTruthy();
        expect(() => ctx.get("foo")).toThrow('cannot get property "foo" without inject');
        await isolated.plugin((ctx: Context) => void expect(ctx.get("foo")).toBeTruthy()).settled();
      })
      .settled();
  });

  it("does not inherit an injected view across an isolation", async () => {
    const root = new Context();
    root.provide("foo", "outer");
    const results: unknown[] = [];
    const access = (ctx: Context) => {
      try {
        return ctx.get("foo");
      } catch (error) {
        return (error as Error).message;
      }
    };
    await root
      .inject(["foo"], async (ctx) => {
        await ctx.plugin((ctx: Context) => void results.push(access(ctx))).settled();
        await ctx.isolate("foo").plugin((ctx: Context) => void results.push(access(ctx))).settled();
      })
      .settled();
    expect(results).toEqual(["outer", 'cannot get property "foo" without inject']);
  });

  it("gives each isolated context its own realm", async () => {
    const root = new Context();
    const callback = vi.fn();
    const dispose = vi.fn();
    const plugin = { inject: ["foo"], apply: () => (callback(), dispose) };
    root.plugin(plugin);
    const ctx1 = root.isolate("foo");
    ctx1.plugin(plugin);
    const ctx2 = root.isolate("foo");
    ctx2.plugin(plugin);

    const provision0 = root.provide("foo", { bar: 100 });
    expect(root.live("foo")).toEqual({ bar: 100 });
    expect(ctx1.live("foo")).toBeUndefined();
    await sleep();
    expect(callback).toHaveBeenCalledTimes(1);

    ctx1.provide("foo", { bar: 200 });
    expect(ctx1.live("foo")).toEqual({ bar: 200 });
    expect(ctx2.live("foo")).toBeUndefined();
    await sleep();
    expect(callback).toHaveBeenCalledTimes(2);

    await provision0.dispose();
    await sleep();
    expect(root.live("foo")).toBeUndefined();
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(ctx1.live("foo")).toEqual({ bar: 200 });
  });

  it("shares a realm between contexts isolated with the same one", async () => {
    const root = new Context();
    const realm = Symbol("shared");
    const callback = vi.fn();
    root.isolate("foo", realm).inject(["foo"], callback);
    root.isolate("foo", realm).inject(["foo"], callback);
    root.isolate("foo", realm).provide("foo", {});
    await sleep();
    expect(callback).toHaveBeenCalledTimes(2);
    expect(root.live("foo")).toBeUndefined();
    expect(root.isolate("foo", realm).realm("foo")).toBe(realm);
  });
});

describe("Interception", () => {
  it("merges metadata from the declaring inject outward in", async () => {
    const root = new Context();
    root.provide("foo", {});
    const metas: unknown[] = [];
    await root
      .intercept("foo", { a: 1, b: 1 })
      .inject({ foo: { b: 2, c: 2 } }, (ctx) => {
        metas.push(ctx.interceptMeta("foo"));
        metas.push(ctx.intercept("foo", { c: 3 }).interceptMeta("foo"));
      })
      .settled();
    expect(metas).toEqual([
      { a: 1, b: 1, c: 2 },
      { a: 1, b: 1, c: 3 },
    ]);
  });

  it("names and levels loggers", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    await root
      .intercept("logger", { name: "custom", level: Level.debug })
      .plugin(function myPlugin(ctx: Context) {
        ctx.logger().debug("hello");
        ctx.logger("named").info("world");
      })
      .settled();
    expect(logs.messages.map((m) => [m.name, m.text])).toEqual([
      ["custom", "hello"],
      ["named", "world"],
    ]);
  });
});
