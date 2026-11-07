import { Context } from "@kardiajs/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Disposed, Timer } from "../src/index.ts";

async function setup() {
  const root = new Context();
  await root.plugin(Timer).settled();
  return root;
}

describe("Timer", () => {
  beforeEach(() => void vi.useFakeTimers());
  afterEach(() => void vi.useRealTimers());

  it("fires a timeout once and removes its spent effect", async () => {
    const root = await setup();
    const callback = vi.fn();
    const fiber = root.inject(["timer"], (ctx) => void ctx.get("timer").timeout(100, callback));
    await vi.advanceTimersByTimeAsync(0);
    expect(fiber.effects().map((effect) => effect.label)).toEqual(["ctx.timeout()"]);
    await vi.advanceTimersByTimeAsync(100);
    expect(callback).toHaveBeenCalledTimes(1);
    expect(fiber.effects()).toEqual([]);
  });

  it("cancels timers when the scheduling plugin stops", async () => {
    const root = await setup();
    const timeout = vi.fn();
    const interval = vi.fn();
    const fiber = root.inject(["timer"], (ctx) => {
      ctx.get("timer").timeout(100, timeout);
      ctx.get("timer").interval(30, interval);
    });
    await vi.advanceTimersByTimeAsync(70);
    expect(interval).toHaveBeenCalledTimes(2);
    await fiber.dispose();
    await vi.advanceTimersByTimeAsync(200);
    expect(timeout).not.toHaveBeenCalled();
    expect(interval).toHaveBeenCalledTimes(2);
  });

  it("stops the plugins using it when the timer itself stops", async () => {
    const root = new Context();
    const provider = root.plugin(Timer);
    const callback = vi.fn();
    const consumer = root.inject(["timer"], (ctx) => void ctx.get("timer").interval(10, callback));
    await vi.advanceTimersByTimeAsync(25);
    expect(callback).toHaveBeenCalledTimes(2);
    await provider.dispose();
    await vi.advanceTimersByTimeAsync(50);
    expect(callback).toHaveBeenCalledTimes(2);
    expect(consumer.state).toBe("pending");
  });

  it("sleeps, and rejects when the caller stops first", async () => {
    const root = await setup();
    const results: unknown[] = [];
    const fiber = root.inject(["timer"], (ctx) => {
      const timer = ctx.get("timer");
      void timer.sleep(10).then(() => results.push("woke"));
      timer.sleep(1000).catch((error) => results.push(error));
    });
    await vi.advanceTimersByTimeAsync(10);
    await fiber.dispose();
    await vi.advanceTimersByTimeAsync(0);
    expect(results).toEqual(["woke", new Disposed()]);
  });

  it("throttles with and without a trailing call", async () => {
    const root = await setup();
    const timer = root.get("timer");
    const trailing = vi.fn();
    const leading = vi.fn();
    const withTrailing = timer.throttle(100, trailing);
    const withoutTrailing = timer.throttle(100, leading, { trailing: false });
    for (const value of [1, 2, 3]) {
      withTrailing(value);
      withoutTrailing(value);
    }
    expect(trailing.mock.calls).toEqual([[1]]);
    await vi.advanceTimersByTimeAsync(100);
    expect(trailing.mock.calls).toEqual([[1], [3]]);
    expect(leading.mock.calls).toEqual([[1]]);
  });

  it("debounces, and ignores calls after dispose", async () => {
    const root = await setup();
    const callback = vi.fn();
    const debounced = root.get("timer").debounce(100, callback);
    debounced("a");
    await vi.advanceTimersByTimeAsync(50);
    debounced("b");
    await vi.advanceTimersByTimeAsync(100);
    expect(callback.mock.calls).toEqual([["b"]]);
    debounced("c");
    await debounced.dispose();
    debounced("d");
    await vi.advanceTimersByTimeAsync(200);
    expect(callback.mock.calls).toEqual([["b"]]);
  });
});
