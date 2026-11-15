import { Context, type RecoveryPolicy } from "@kardiajs/core";
import { afterEach, describe, expect, it } from "vitest";

import { Timer } from "#/index";

const roots: Context[] = [];
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
afterEach(async () => { for (const root of roots.splice(0)) await root.fiber.dispose(); });

async function setup() {
  const root = new Context();
  roots.push(root);
  await root.plugin(Timer).settled();
  return root;
}

async function until(predicate: () => boolean) {
  const deadline = Date.now() + 2_000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error("timer did not settle");
    await sleep(2);
  }
}

const schedulers = {
  timeout: (timer: Timer, callback: () => unknown) => { timer.timeout(5, callback); },
  interval: (timer: Timer, callback: () => unknown) => { timer.interval(5, callback); },
  throttle: (timer: Timer, callback: () => unknown) => { const run = timer.throttle(5, callback); run(); run(); },
  debounce: (timer: Timer, callback: () => unknown) => { const run = timer.debounce(5, callback); run(); run(); },
};

describe("real timer callback isolation", () => {
  for (const [name, schedule] of Object.entries(schedulers)) {
    for (const asynchronous of [false, true]) {
      it(`${name} contains ${asynchronous ? "rejections" : "throws"} without restarting its plugin`, async () => {
        const root = await setup();
        let progress = 0;
        const healthy = root.inject(["timer"], (ctx) => {
          ctx.get("timer").interval(2, () => progress++);
        });
        let loads = 0;
        let calls = 0;
        const broken = root.plugin({ name: "broken", inject: ["timer"], apply: (ctx: Context) => {
          loads++;
          schedule(ctx.get("timer"), () => {
            calls++;
            if (asynchronous) return Promise.reject(new Error("callback failed"));
            throw new Error("callback failed");
          });
        } });
        broken.setRecovery({ type: "retry", delay: 5 });
        await broken.settled();
        await until(() => root.logBuffer().some((entry) => entry.text.includes("callback failed")) && progress > 2);
        expect(root.logBuffer().filter((entry) => entry.text.includes("callback failed")).every((entry) => entry.fiber === broken.id)).toBe(true);
        expect(loads).toBe(1);
        expect(broken.state).toBe("active");
        expect(healthy.state).toBe("active");
        await broken.dispose();
        const stopped = calls;
        const before = progress;
        await sleep(15);
        expect(calls).toBe(stopped);
        expect(progress).toBeGreaterThan(before);
      });
    }

    it(`${name} waits for callbacks already in flight during disposal`, async () => {
      const root = await setup();
      let started = 0;
      let completed = 0;
      let release!: () => void;
      const gate = new Promise<void>((resolve) => { release = resolve; });
      const fiber = root.inject(["timer"], (ctx) => {
        schedule(ctx.get("timer"), async () => { started++; await gate; completed++; });
      });
      try {
        await until(() => started > 0);
        let disposed = false;
        const disposal = fiber.dispose().then(() => { disposed = true; });
        await sleep(10);
        const stopping = started;
        expect(disposed).toBe(false);
        expect(completed).toBe(0);
        release();
        await disposal;
        expect(completed).toBe(stopping);
        await sleep(10);
        expect(started).toBe(stopping);
      } finally {
        release();
      }
    });
  }

  for (const mode of ["manual", "retry", "host"] as const) {
    it(`${mode} recovers explicitly failed timer owners only after their callbacks finish`, async () => {
      const root = await setup();
      let progress = 0;
      const healthy = root.inject(["timer"], (ctx) => { ctx.get("timer").interval(2, () => progress++); });
      let loads = 0;
      let running = 0;
      let finished = 0;
      const fiber = root.inject(["timer"], (ctx) => {
        expect(running).toBe(0);
        loads++;
        if (loads > 3) return;
        ctx.get("timer").timeout(5, async () => {
          running++;
          ctx.fiber.fail(new Error("instance no longer usable"));
          await sleep(10);
          running--;
          finished++;
        });
      });
      const policy: RecoveryPolicy = mode === "host"
        ? { type: "host", handler: () => ({ type: "retry", delay: 15 }) }
        : mode === "retry" ? { type: "retry", delay: 15 } : { type: "manual" };
      fiber.setRecovery(policy);
      for (let round = 1; round <= 3; round++) {
        await until(() => fiber.state === "failed" && finished === round);
        expect(running).toBe(0);
        expect(healthy.state).toBe("active");
        if (mode === "manual") {
          await sleep(20);
          expect(loads).toBe(round);
          await fiber.update(fiber.config, true);
        }
        await until(() => loads === round + 1);
      }
      await until(() => fiber.state === "active");
      expect(finished).toBe(3);
      expect(progress).toBeGreaterThan(3);
    });
  }
});
