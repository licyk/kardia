import { describe, expect, it, vi } from "vitest";

import { Context, type FiberState, type StandardSchemaV1, ValidationError } from "#/index";
import { captureLogs, sleep, withTimers } from "./helpers.ts";

describe("Fibers", () => {
  it("keep a load running after its dependency leaves (inertia)", withTimers(async (root) => {
    const provision = root.provide("foo", 1);
    const fiber = root.inject(["foo"], async () => {
      await sleep(1000);
      return () => sleep(1000);
    });
    await vi.advanceTimersByTimeAsync(400);
    expect(fiber.state).toBe("loading");
    void provision.dispose();
    await vi.advanceTimersByTimeAsync(400);
    expect(fiber.state).toBe("loading");
    await vi.advanceTimersByTimeAsync(400);
    expect(fiber.state).toBe("unloading");
    root.provide("foo", 1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fiber.state).toBe("loading");
    await vi.advanceTimersByTimeAsync(1000);
    expect(fiber.state).toBe("active");
  }));

  it("reload once a superseding binding appears during a load", withTimers(async (root) => {
    const provision = root.provide("foo", 1);
    const fiber = root.inject(["foo"], async () => {
      await sleep(1000);
      return () => sleep(1000);
    });
    await vi.advanceTimersByTimeAsync(400);
    void provision.dispose();
    await vi.advanceTimersByTimeAsync(400);
    root.provide("foo", 2);
    await vi.advanceTimersByTimeAsync(400);
    expect(fiber.state).toBe("unloading");
    await vi.advanceTimersByTimeAsync(2000);
    expect(fiber.state).toBe("active");
    expect(fiber.ctx.get("foo")).toBe(2);
  }));

  it("fail alone when apply throws", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    const callback = vi.fn();
    const apply = (ctx: Context, config: { foo?: boolean } | undefined) => {
      ctx.on("custom-event", callback);
      if (!config?.foo) throw new Error("plugin error");
    };
    const fiber1 = root.plugin(apply);
    const fiber2 = root.plugin(apply, { foo: true });
    await sleep();
    expect(fiber1.state).toBe("failed");
    expect(fiber1.error?.message).toBe("plugin error");
    expect(fiber2.state).toBe("active");
    expect(logs.errors()).toHaveLength(1);
    root.emit("custom-event");
    expect(callback).toHaveBeenCalledTimes(1);
  });

  it("stay failed when a dependency changes", async () => {
    const root = new Context();
    captureLogs(root);
    const apply = vi.fn(() => {
      throw new Error("boom");
    });
    const provision = root.provide("foo", 1);
    const fiber = root.inject(["foo"], apply);
    await sleep();
    expect(fiber.state).toBe("failed");
    await provision.dispose();
    root.provide("foo", 2);
    await sleep();
    expect(apply).toHaveBeenCalledTimes(1);
    expect(fiber.state).toBe("failed");
  });

  it("recover through update", async () => {
    const root = new Context();
    captureLogs(root);
    const apply = vi.fn((): void => {
      throw new Error("boom");
    });
    root.provide("foo", 1);
    const fiber = root.inject(["foo"], apply);
    await sleep();
    apply.mockImplementationOnce(() => {});
    await fiber.update(undefined);
    expect(apply).toHaveBeenCalledTimes(2);
    expect(fiber.state).toBe("active");
  });

  it("report a failed reload to the update caller", async () => {
    const root = new Context();
    captureLogs(root);
    const apply = vi.fn((_ctx: Context, config: { message?: string }) => {
      if (config?.message) throw new Error(config.message);
    });
    const fiber = root.plugin(apply, {});
    await fiber.settled();
    await expect(fiber.update({ message: "retry failure" })).rejects.toThrow("retry failure");
    expect(fiber.state).toBe("failed");
  });

  // the fiber reports the failure itself, so a caller that drops the result
  // must not cause an unhandled rejection (vitest fails the run on one)
  it("do not leak a dropped failed update", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    const apply = vi.fn();
    const fiber = root.plugin(apply);
    await fiber.settled();
    apply.mockImplementationOnce(() => {
      throw new Error("boom");
    });
    void fiber.update({});
    await sleep();
    expect(fiber.state).toBe("failed");
    expect(logs.errors()).toHaveLength(1);
  });

  it("log a failing inverse and still stop", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    const fiber = root.plugin(() => () => {
      throw new Error("test");
    });
    await fiber.settled();
    await expect(fiber.dispose()).resolves.toBeUndefined();
    await sleep();
    expect(fiber.state).toBe("disposed");
    expect(logs.errors()).toHaveLength(1);
  });

  it.each([
    ["pending", () => {}, ["pending", "disposed"]],
    ["active", undefined, ["active", "unloading", "disposed"]],
  ] as const)("move from %s to disposed", async (_, __, expected) => {
    const root = new Context();
    const fiber = expected[0] === "pending" ? root.inject(["never-provided"], () => {}) : root.plugin(() => {});
    await fiber.settled();
    const states: FiberState[] = [fiber.state];
    root.on("internal/status", (target, _old, state) => {
      if (target.id === fiber.id) states.push(state);
    });
    await fiber.dispose();
    expect(states).toEqual(expected);
  });

  it("update their config, restarting", async () => {
    const root = new Context();
    const callback = vi.fn();
    const fiber = root.plugin(callback, { msg: "hello" });
    await fiber.settled();
    await fiber.update({ msg: "world" });
    expect(callback.mock.calls.map((call) => call[1])).toEqual([{ msg: "hello" }, { msg: "world" }]);
    expect(fiber.config).toEqual({ msg: "world" });
  });

  it("restart", async () => {
    const root = new Context();
    const callback = vi.fn();
    const fiber = root.plugin(callback);
    await fiber.settled();
    await fiber.restart();
    expect(callback).toHaveBeenCalledTimes(2);
    expect(fiber.state).toBe("active");
  });

  it("let an update hook take an update over", async () => {
    const root = new Context();
    const applied: unknown[] = [];
    const received: unknown[] = [];
    const fiber = root.plugin((ctx: Context, config: { value: number }) => {
      applied.push(config);
      ctx.onUpdate((config) => {
        received.push(config);
        return config.value < 10;
      });
    }, { value: 1 });
    await fiber.settled();
    await fiber.update({ value: 2 });
    expect(applied).toEqual([{ value: 1 }]);
    expect(received).toEqual([{ value: 2 }]);
    expect(fiber.config).toEqual({ value: 2 });
    await fiber.update({ value: 20 });
    expect(applied).toEqual([{ value: 1 }, { value: 20 }]);
  });

  it("emit internal/commit unless the update is not saved", async () => {
    const root = new Context();
    const commits: unknown[] = [];
    root.on("internal/commit", (_fiber, config) => void commits.push(config));
    const fiber = root.plugin(() => {}, 1);
    await fiber.settled();
    await fiber.update(2);
    await fiber.update(3, true);
    expect(commits).toEqual([2]);
  });

  it("warn about dependency cycles", async () => {
    const root = new Context();
    const logs = captureLogs(root);
    root.plugin({ name: "a", inject: ["b"], provide: "a", apply: (ctx: Context) => void ctx.provide("a", {}) });
    root.plugin({ name: "b", inject: ["a"], provide: "b", apply: (ctx: Context) => void ctx.provide("b", {}) });
    await sleep();
    expect(logs.warnings().map((message) => message.text)).toEqual([
      expect.stringContaining("dependency cycle: b -> a -> b"),
    ]);
  });
});

describe("Config validation", () => {
  const Config: StandardSchemaV1<{ ok: boolean }, { ok: boolean; resolved: true }> = {
    "~standard": {
      version: 1,
      vendor: "test",
      validate: (value: any) => (value?.ok ? { value: { ...value, resolved: true } } : { issues: [{ message: "not ok" }] }),
    },
  };

  it("fails the first load", async () => {
    const root = new Context();
    captureLogs(root);
    const apply = vi.fn();
    const fiber = root.plugin({ apply, Config }, { ok: false });
    await sleep();
    expect(apply).not.toHaveBeenCalled();
    expect(fiber.state).toBe("failed");
    expect(fiber.error).toBeInstanceOf(ValidationError);
  });

  it("passes the decoded config to apply and keeps the source", async () => {
    const root = new Context();
    const apply = vi.fn();
    const fiber = root.plugin({ apply, Config }, { ok: true });
    await fiber.settled();
    expect(apply.mock.calls[0][1]).toEqual({ ok: true, resolved: true });
    expect(fiber.config).toEqual({ ok: true });
  });

  it("keeps a running plugin when an update is rejected", async () => {
    const root = new Context();
    const apply = vi.fn();
    const fiber = root.plugin({ apply, Config }, { ok: true });
    await fiber.settled();
    expect(() => fiber.update({ ok: false })).toThrow("invalid config");
    await sleep();
    expect(apply).toHaveBeenCalledTimes(1);
    expect(fiber.state).toBe("active");
    expect(fiber.config).toEqual({ ok: true });
  });

  it("formats issues with their paths", () => {
    const error = new ValidationError([
      { message: "not a number", path: ["items", 0] },
      { message: "required", path: [] },
    ]);
    expect(error.message).toBe("invalid config:\n  - not a number (at items.0)\n  - required");
  });
});
