import { describe, expect, it, vi } from "vitest";

import { Context, Interrupted } from "../src/index.ts";
import { captureLogs, sleep, withTimers } from "./helpers.ts";

describe("Effects", () => {
  it("are disposed with their plugin", async () => {
    const root = new Context();
    const dispose = vi.fn();
    const fiber = root.plugin((ctx) => {
      ctx.effect(() => dispose, "test");
    });
    await fiber.settled();
    expect(fiber.effects()).toEqual([{ label: "test", children: [] }]);
    expect(dispose).not.toHaveBeenCalled();
    await fiber.dispose();
    expect(dispose).toHaveBeenCalledTimes(1);
    await fiber.dispose();
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("can be disposed manually, once", () => {
    const root = new Context();
    const dispose = vi.fn();
    const handle = root.effect(() => dispose);
    expect(root.fiber.effects()).toEqual([{ label: "anonymous", children: [] }]);
    void handle.dispose();
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(handle.active).toBe(false);
    void handle.dispose();
    expect(dispose).toHaveBeenCalledTimes(1);
    expect(root.fiber.effects()).toEqual([]);
  });

  it("run collected inverses newest first, and adopted effects with them", () => {
    const root = new Context();
    const seq: number[] = [];
    const handle = root.effect((scope) => {
      scope.defer(() => void seq.push(1));
      scope.adopt(root.on("custom-event", () => {}));
      scope.defer(() => void seq.push(2));
      scope.adopt(
        root.effect((inner) => {
          inner.adopt(root.on("custom-event", () => {}));
          inner.defer(() => void seq.push(3));
        }),
      );
    });
    root.on("custom-event", () => {});
    expect(root.fiber.effects()).toEqual([
      {
        label: "anonymous",
        children: [
          { label: 'ctx.on("custom-event")', children: [] },
          { label: "anonymous", children: [{ label: 'ctx.on("custom-event")', children: [] }] },
        ],
      },
      { label: 'ctx.on("custom-event")', children: [] },
    ]);
    void handle.dispose();
    expect(seq).toEqual([3, 2, 1]);
  });

  it("collect a disposer an async body returns", withTimers(async (root) => {
    const seq: number[] = [];
    const handle = root.effect(async () => {
      await sleep(100);
      seq.push(1);
      return () => void seq.push(2);
    });
    await vi.advanceTimersByTimeAsync(100);
    expect(seq).toEqual([1]);
    await handle.dispose();
    expect(seq).toEqual([1, 2]);
  }));

  it("wait for an async body before disposing it", withTimers(async (root) => {
    const seq: number[] = [];
    const handle = root.effect(async () => {
      await sleep(100);
      seq.push(1);
      return () => void seq.push(2);
    });
    const disposing = handle.dispose();
    expect(seq).toEqual([]);
    await vi.advanceTimersByTimeAsync(100);
    await disposing;
    expect(seq).toEqual([1, 2]);
  }));

  it("end an async body at a checkpoint once disposed", withTimers(async (root) => {
    const seq: number[] = [];
    const handle = root.effect(async (scope) => {
      for (const step of [1, 2, 3]) {
        await sleep(100);
        scope.checkpoint();
        seq.push(step);
        scope.defer(() => void seq.push(-step));
      }
    });
    await vi.advanceTimersByTimeAsync(150);
    expect(seq).toEqual([1]);
    const disposing = handle.dispose();
    await vi.advanceTimersByTimeAsync(300);
    await disposing;
    expect(seq).toEqual([1, -1]);
    await expect(handle.ready()).resolves.toBeUndefined();
  }));

  it("roll back a synchronous body that throws", () => {
    const root = new Context();
    const seq: number[] = [];
    expect(() =>
      root.effect((scope) => {
        scope.defer(() => void seq.push(1));
        throw new Error("test");
      }),
    ).toThrow("test");
    expect(seq).toEqual([1]);
    expect(root.fiber.effects()).toEqual([]);
  });

  it("roll back and report an async body that fails", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    const seq: number[] = [];
    const handle = root.effect(async (scope) => {
      scope.defer(() => void seq.push(1));
      await sleep();
      throw new Error("test");
    });
    await expect(handle.ready()).rejects.toThrow("test");
    expect(seq).toEqual([1]);
    expect(logs.errors()).toHaveLength(1);
    await handle.dispose();
    expect(seq).toEqual([1]);
  });

  it("do not treat Interrupted as a failure", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    const handle = root.effect(async () => {
      throw new Interrupted();
    });
    await expect(handle.ready()).resolves.toBeUndefined();
    expect(logs.errors()).toHaveLength(0);
  });

  it("run every inverse and keep the first error", async () => {
    const root = new Context();
    const seq: number[] = [];
    const handle = root.effect((scope) => {
      scope.defer(() => void seq.push(1));
      scope.defer(() => {
        throw new Error("second");
      });
      scope.defer(async () => {
        await sleep();
        throw new Error("first");
      });
    });
    await expect(handle.dispose()).rejects.toThrow("first");
    expect(seq).toEqual([1]);
  });

  it("are refused on a disposed plugin", async () => {
    const root = new Context();
    const callback = vi.fn();
    const fiber = root.plugin((ctx) => () => {
      expect(() => ctx.plugin(callback)).toThrow("inactive context");
      expect(() => ctx.effect(() => {})).toThrow("inactive context");
      expect(() => ctx.on("custom-event", () => {})).toThrow("inactive context");
    });
    await fiber.settled();
    await fiber.dispose();
    expect(callback).not.toHaveBeenCalled();
  });
});
