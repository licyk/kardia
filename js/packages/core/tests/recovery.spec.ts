import { afterEach, describe, expect, it } from "vitest";

import { Context, type RecoveryPolicy, ValidationError } from "#/index";
import { captureLogs, sleep } from "./helpers.ts";

const roots: Context[] = [];
afterEach(async () => { for (const root of roots.splice(0)) await root.fiber.dispose(); });

function setup() {
  const root = new Context();
  roots.push(root);
  const logs = captureLogs(root);
  return { root, logs };
}

async function until(predicate: () => boolean) {
  const deadline = Date.now() + 2_000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error("recovery did not settle");
    await sleep(2);
  }
}

describe("instance failure and recovery", () => {
  for (const mode of ["manual", "retry", "host"] as const) {
    it(`${mode}: repeated failures clean up before recovering while unrelated plugins keep working`, async () => {
      const { root } = setup();
      let progress = 0;
      const healthy = root.plugin((ctx) => {
        ctx.provide("healthy", () => ++progress);
        const timer = setInterval(() => ++progress, 2);
        ctx.onDispose(() => clearInterval(timer));
      });
      let instances = 0;
      let live = 0;
      let cleaned = 0;
      let decisions = 0;
      let commits = 0;
      root.on("internal/commit", () => commits++);
      const failed = root.plugin((ctx) => {
        expect(live).toBe(0);
        live++;
        instances++;
        ctx.provide("test", instances);
        ctx.onDispose(async () => { await sleep(5); live--; cleaned++; });
      });
      const policy: RecoveryPolicy = mode === "host"
        ? { type: "host", handler: ({ fiber, error }) => {
          expect(fiber).toBe(failed);
          expect(error.message).toBe("failed instance");
          expect(live).toBe(0);
          decisions++;
          return { type: "retry", delay: 15 };
        } }
        : mode === "retry" ? { type: "retry", delay: 15 } : { type: "manual" };
      failed.setRecovery(policy);
      const consumer = root.inject(["test"], () => {});
      await Promise.all([healthy.settled(), failed.settled(), consumer.settled()]);
      const healthyService = root.get("healthy");
      for (let count = 1; count <= 3; count++) {
        const before = progress;
        failed.fail(new Error("failed instance"));
        await until(() => failed.state === "failed");
        expect(root.live("test")).toBeUndefined();
        expect(consumer.state).toBe("pending");
        expect(cleaned).toBe(count);
        if (mode === "manual") {
          await sleep(25);
          expect(instances).toBe(count);
          expect(failed.state).toBe("failed");
          await failed.update(failed.config, true);
        }
        await until(() => failed.state === "active" && consumer.state === "active");
        expect(instances).toBe(count + 1);
        expect(live).toBe(1);
        expect(progress).toBeGreaterThan(before);
        expect(healthy.state).toBe("active");
        expect(root.get("healthy")).toBe(healthyService);
      }
      expect(commits).toBe(0);
      expect(decisions).toBe(mode === "host" ? 3 : 0);
    });
  }

  it("recovers failed loads automatically without update calls from the test", async () => {
    const { root } = setup();
    let loads = 0;
    let cleanups = 0;
    const fiber = root.plugin((ctx) => {
      expect(cleanups).toBe(loads);
      ctx.onDispose(() => { cleanups++; });
      if (++loads < 4) throw new Error("load failed");
    });
    fiber.setRecovery({ type: "retry", delay: 5 });
    await until(() => fiber.state === "active");
    expect(loads).toBe(4);
    expect(cleanups).toBe(3);
  });

  it("manual is the default and a plain restart does not clear failure", async () => {
    const { root } = setup();
    let loads = 0;
    const fiber = root.plugin(() => { loads++; throw new Error("manual"); });
    await expect(fiber.settled()).rejects.toThrow("manual");
    await sleep(20);
    await expect(fiber.restart()).rejects.toThrow("manual");
    expect(loads).toBe(1);
  });

  it("a failed restart keeps supervision active for the new revision", async () => {
    const { root } = setup();
    let loads = 0;
    const fiber = root.plugin(() => { if (++loads === 1) throw new Error("once"); });
    fiber.setRecovery({ type: "retry", delay: 15 });
    await expect(fiber.settled()).rejects.toThrow("once");
    await expect(fiber.restart()).rejects.toThrow("once");
    expect(loads).toBe(1);
    await until(() => fiber.state === "active");
    expect(loads).toBe(2);
  });

  for (const mode of ["retry", "host"] as const) {
    it(`${mode} does not retry invalid configuration`, async () => {
      const { root } = setup();
      let validations = 0;
      const fiber = root.plugin({
        Config: { "~standard": { version: 1, vendor: "test", validate: () => {
          validations++;
          return { issues: [{ message: "invalid" }] };
        } } },
        apply: () => { throw new Error("must not run"); },
      });
      fiber.setRecovery(mode === "retry" ? { type: "retry", delay: 5 } : {
        type: "host", handler: () => ({ type: "retry", delay: 5 }),
      });
      await expect(fiber.settled()).rejects.toBeInstanceOf(ValidationError);
      await sleep(25);
      expect(validations).toBe(1);
    });
  }

  for (const action of ["dispose", "manual", "update", "replace"] as const) {
    it(`${action} cancels a pending retry`, async () => {
      const { root } = setup();
      let loads = 0;
      const plugin = () => { if (++loads === 1) throw new Error("once"); };
      const fiber = root.plugin(plugin);
      fiber.setRecovery({ type: "retry", delay: 35 });
      await expect(fiber.settled()).rejects.toThrow("once");
      if (action === "dispose") await fiber.dispose();
      if (action === "manual") fiber.setRecovery({ type: "manual" });
      if (action === "update") await fiber.update(undefined);
      if (action === "replace") { root.registry.replace(plugin, () => { loads++; }); await fiber.settled(); }
      await sleep(55);
      expect(loads).toBe(action === "update" || action === "replace" ? 2 : 1);
    });
  }

  it("an asynchronous host decision is cancelled on policy change or removal", async () => {
    const { root } = setup();
    for (const remove of [false, true]) {
      let loads = 0;
      let signal: AbortSignal | undefined;
      let decide!: (action: { type: "retry"; delay: number }) => void;
      const fiber = root.plugin(() => { loads++; throw new Error("host"); });
      fiber.setRecovery({ type: "host", handler: (failure) => {
        signal = failure.signal;
        return new Promise((resolve) => { decide = resolve; });
      } });
      await expect(fiber.settled()).rejects.toThrow("host");
      if (remove) await fiber.dispose();
      else fiber.setRecovery({ type: "manual" });
      expect(signal?.aborted).toBe(true);
      decide({ type: "retry", delay: 5 });
      await sleep(20);
      expect(loads).toBe(1);
    }
  });

  it.each([false, true])("contains host handler errors (async=%s)", async (async) => {
    const { root, logs } = setup();
    const fiber = root.plugin(() => { throw new Error("instance"); });
    fiber.setRecovery({ type: "host", handler: () => {
      if (async) return Promise.reject(new Error("host decision"));
      throw new Error("host decision");
    } });
    await expect(fiber.settled()).rejects.toThrow("instance");
    await sleep(10);
    expect(logs.errors().length).toBe(2);
    expect(fiber.state).toBe("failed");
    const healthy = root.plugin((ctx) => ctx.provide("ok", true));
    await healthy.settled();
    expect(root.get("ok")).toBe(true);
  });

  it("supports keep-failed and switching a failed fiber to automatic recovery", async () => {
    const { root } = setup();
    let loads = 0;
    const fiber = root.plugin(() => { if (++loads === 1) throw new Error("once"); });
    fiber.setRecovery({ type: "host", handler: () => ({ type: "keep-failed" }) });
    await expect(fiber.settled()).rejects.toThrow("once");
    await sleep(15);
    expect(loads).toBe(1);
    fiber.setRecovery({ type: "retry", delay: 5 });
    await until(() => fiber.state === "active");
    expect(loads).toBe(2);
  });

  it("rejects unbounded timer delays and recovery of the root", () => {
    const { root } = setup();
    const fiber = root.plugin(() => {});
    for (const delay of [0, -1, NaN, Infinity, 2_147_483_648]) {
      expect(() => fiber.setRecovery({ type: "retry", delay })).toThrow(RangeError);
    }
    expect(() => root.fiber.setRecovery({ type: "retry", delay: 5 })).toThrow("root");
    expect(() => root.fiber.fail(new Error("root"))).toThrow("root");
  });
});
