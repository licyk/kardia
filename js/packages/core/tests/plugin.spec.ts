import { describe, expect, it, vi } from "vitest";

import { Context, definePlugin, Service } from "#/index";
import { listenerCount } from "./helpers.ts";

describe("Plugins", () => {
  it("apply a function plugin with its config", async () => {
    const root = new Context();
    const callback = vi.fn();
    await root.plugin(callback, { foo: "bar" }).settled();
    expect(callback).toHaveBeenCalledTimes(1);
    expect(callback.mock.calls[0][1]).toEqual({ foo: "bar" });
  });

  it("apply an object plugin", async () => {
    const root = new Context();
    const apply = vi.fn();
    await root.plugin({ apply }, { bar: "foo" }).settled();
    expect(apply.mock.calls[0][1]).toEqual({ bar: "foo" });
  });

  it("instantiate a class plugin per fiber", async () => {
    const root = new Context();
    const created: unknown[] = [];
    class Foo {
      constructor(_ctx: Context, config: unknown) {
        created.push(config);
      }
    }
    await root.plugin(Foo, 1).settled();
    await root.plugin(Foo, 2).settled();
    expect(created).toEqual([1, 2]);
  });

  it("refuse invalid plugins", () => {
    const root = new Context();
    expect(() => root.plugin(undefined as any)).toThrow();
    expect(() => root.plugin({} as any)).toThrow();
    expect(() => root.plugin({ apply: {} } as any)).toThrow();
  });

  it("are named after their function, class or name", async () => {
    const root = new Context();
    const names: string[] = [];
    await root.plugin((ctx) => void names.push(ctx.fiber.name)).settled();
    await root.plugin(function foo(ctx: Context) {
      names.push(ctx.fiber.name);
    }).settled();
    await root.plugin({ name: "bar", apply: (ctx: Context) => void names.push(ctx.fiber.name) }).settled();
    await root.plugin(
      class Qux {
        constructor(ctx: Context) {
          names.push(ctx.fiber.name);
        }
      },
    ).settled();
    expect(names).toEqual(["root", "foo", "bar", "Qux"]);
    expect(String(root)).toBe("Context <root>");
  });

  it("nest, and dispose their children with them", async () => {
    const root = new Context();
    const callback = vi.fn();
    const plugin = async (ctx: Context) => {
      ctx.on("custom-event", callback);
      await ctx
        .plugin(async (ctx: Context) => {
          ctx.on("custom-event", callback);
          await ctx.plugin((ctx: Context) => void ctx.on("custom-event", callback)).settled();
        })
        .settled();
    };
    root.on("custom-event", callback);
    const fiber = root.plugin(plugin);
    await fiber.settled();
    expect(root.registry.size).toBe(3);
    root.emit("custom-event");
    expect(callback).toHaveBeenCalledTimes(4);

    callback.mockClear();
    await fiber.dispose();
    expect(root.registry.size).toBe(0);
    root.emit("custom-event");
    expect(callback).toHaveBeenCalledTimes(1);
    expect(listenerCount(root, "custom-event")).toBe(1);
  });

  it("run a function returned by apply when they stop", async () => {
    const root = new Context();
    const dispose = vi.fn();
    const fiber = root.plugin(() => dispose);
    await fiber.settled();
    await fiber.dispose();
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("stop with the root fiber, which keeps running", async () => {
    const root = new Context();
    const dispose = vi.fn();
    const fiber = root.plugin(() => dispose);
    await fiber.settled();
    expect(root.fiber.effects()).toHaveLength(1);
    await root.fiber.dispose();
    expect(fiber.state).toBe("disposed");
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(root.fiber.effects()).toHaveLength(0);
    expect(root.fiber.state).toBe("active");
    await root.plugin(() => {}).settled();
  });

  it("type their context by inject with definePlugin", async () => {
    const root = new Context();
    root.provide("foo", 1);
    const seen: unknown[] = [];
    const plugin = definePlugin({
      inject: ["foo"],
      apply(ctx) {
        seen.push(ctx.get("foo"));
        // @ts-expect-error undeclared service
        expect(() => ctx.get("bar")).toThrow('cannot get property "bar" without inject');
      },
    });
    await root.plugin(plugin).settled();
    expect(seen).toEqual([1]);
  });

  it("can be services that provide themselves", async () => {
    class Foo extends Service {
      constructor(ctx: Context) {
        super(ctx, "foo");
      }
    }
    const root = new Context();
    const fiber = root.plugin(Foo);
    await fiber.settled();
    expect(root.get("foo")).toBeInstanceOf(Foo);
    await fiber.dispose();
    expect(() => root.get("foo")).toThrow("inactive context");
  });
});
