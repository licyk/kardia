import { describe, expect, it, vi } from "vitest";

import { Context, type StandardSchemaV1 } from "../src/index.ts";
import { sleep } from "./helpers.ts";

describe("registry.replace", () => {
  it("swaps the implementation behind a fiber in place", async () => {
    const root = new Context();
    const disposeOld = vi.fn();
    const old = vi.fn(() => disposeOld);
    const next = vi.fn();
    const fiber = root.plugin(old, { foo: 1 });
    await fiber.settled();

    const replaced = root.registry.replace(old, next);
    await Promise.all(replaced.map((fiber) => fiber.settled()));

    expect(replaced.map((fiber) => fiber.id)).toEqual([fiber.id]);
    expect(disposeOld).toHaveBeenCalledTimes(1);
    expect(next.mock.calls[0][1]).toEqual({ foo: 1 });
    expect(fiber.state).toBe("active");
    expect(fiber.plugin).toBe(next);
    expect(root.registry.keys()).toEqual([next]);
  });

  it("forwards later instantiations of the old plugin", async () => {
    const root = new Context();
    const old = vi.fn();
    const next = vi.fn();
    await root.plugin(old).settled();
    root.registry.replace(old, next);
    await sleep();
    await root.plugin(old).settled();
    expect(old).toHaveBeenCalledTimes(1);
    expect(next).toHaveBeenCalledTimes(2);
    expect(root.registry.has(old)).toBe(true);
    expect(root.registry.fibers(old)).toHaveLength(2);
  });

  it("emits internal/replace for each moved fiber", async () => {
    const root = new Context();
    const old = () => {};
    const next = () => {};
    const seen: unknown[] = [];
    root.on("internal/replace", (fiber) => void seen.push(fiber.plugin));
    await root.plugin(old).settled();
    await root.plugin(old).settled();
    root.registry.replace(old, next);
    expect(seen).toEqual([next, next]);
  });

  it("validates the config with the new schema", async () => {
    const root = new Context();
    const schema = (version: number): StandardSchemaV1 => ({
      "~standard": { version: 1, vendor: "test", validate: (value: any) => ({ value: { ...value, version } }) },
    });
    const apply = vi.fn();
    const old = { Config: schema(1), apply() {} };
    const next = { Config: schema(2), apply };
    await root.plugin(old, { foo: 1 }).settled();
    await Promise.all(root.registry.replace(old, next).map((fiber) => fiber.settled()));
    expect(apply.mock.calls[0][1]).toEqual({ foo: 1, version: 2 });
  });

  it("takes the dependencies of the new plugin", async () => {
    const root = new Context();
    const old = { apply() {} };
    const apply = vi.fn();
    const fiber = root.plugin(old);
    await fiber.settled();
    root.registry.replace(old, { inject: ["dep"], apply });
    await fiber.settled();
    expect([...fiber.inject.keys()]).toEqual(["dep"]);
    expect(fiber.state).toBe("pending");
    root.provide("dep", {});
    await sleep();
    await fiber.settled();
    expect(fiber.state).toBe("active");
    expect(apply).toHaveBeenCalledTimes(1);
  });

  it("recovers a failed fiber", async () => {
    const root = new Context();
    root._root.logger.exporters.set(-1, { export: () => {} });
    const old = () => {
      throw new Error("boom");
    };
    const next = vi.fn();
    const fiber = root.plugin(old);
    await expect(fiber.settled()).rejects.toThrow("boom");
    root.registry.replace(old, next);
    await fiber.settled();
    expect(fiber.state).toBe("active");
  });

  it("reloads the consumers of a replaced provider", async () => {
    const root = new Context();
    const old = (ctx: Context) => void ctx.provide("replaced", "old");
    const next = (ctx: Context) => void ctx.provide("replaced", "next");
    const values: unknown[] = [];
    await root.plugin(old).settled();
    const consumer = root.inject(["replaced"], (ctx) => void values.push(ctx.get("replaced")));
    await consumer.settled();
    const [fiber] = root.registry.replace(old, next);
    await fiber.settled();
    await sleep();
    await consumer.settled();
    expect(values).toEqual(["old", "next"]);
  });
});
