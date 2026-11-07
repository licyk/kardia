import { describe, expect, it, vi } from "vitest";

import { AccessError, Context, type Fiber, KardiaError, Service } from "../src/index.ts";
import { captureLogs, sleep } from "./helpers.ts";

class Counter extends Service {
  value = 0;

  /** An effect of the caller: undone when the caller stops. */
  increase() {
    return this.caller.effect(() => {
      this.value++;
      return () => void this.value--;
    });
  }
}

describe("Services", () => {
  it("start a consumer once its dependency is provided, and stop it when it leaves", async () => {
    const root = new Context();
    const dispose = vi.fn();
    const apply = vi.fn(() => dispose);
    const fiber = root.inject(["foo"], apply);
    await sleep();
    expect(fiber.state).toBe("pending");
    const provision = root.provide("foo", {});
    await fiber.settled();
    expect(apply).toHaveBeenCalledTimes(1);
    await provision.dispose();
    await fiber.settled();
    expect(fiber.state).toBe("pending");
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("restart a consumer when its dependency is replaced", async () => {
    const root = new Context();
    const values: unknown[] = [];
    const provider = root.plugin((ctx: Context, config: number) => void ctx.provide("foo", config), 1);
    const consumer = root.inject(["foo"], (ctx) => void values.push(ctx.get("foo")));
    await provider.settled();
    await consumer.settled();
    await provider.update(2);
    await consumer.settled();
    expect(values).toEqual([1, 2]);
  });

  it("wait for a provider's load", async () => {
    const root = new Context();
    const callback = vi.fn();
    root.inject(["foo"], callback);
    let resolve!: () => void;
    root.plugin(async (ctx: Context) => {
      ctx.provide("foo", {});
      await new Promise<void>((r) => (resolve = r));
    });
    await sleep();
    expect(callback).not.toHaveBeenCalled();
    resolve();
    await sleep();
    expect(callback).toHaveBeenCalledTimes(1);
  });

  it("start when every dependency is there, in any order", async () => {
    const root = new Context();
    const started: string[] = [];
    const provider = (name: string, inject: string[]) => ({
      name,
      inject,
      apply: (ctx: Context) => {
        ctx.provide(name, {});
        started.push(name);
      },
    });
    root.plugin(provider("foo", ["qux"]));
    root.plugin(provider("bar", ["foo", "qux"]));
    root.plugin(provider("qux", []));
    await sleep();
    expect(started).toEqual(["qux", "foo", "bar"]);
  });

  it("refuse a second provider in the same realm", async () => {
    const root = new Context();
    root.plugin(function first(ctx: Context) {
      ctx.provide("foo", 1);
    });
    await sleep();
    expect(() => root.provide("foo", 2)).toThrow(KardiaError);
    expect(() => root.provide("foo", 2)).toThrow('service "foo" has been registered at <first>');
  });

  it("check access against the plugin's inject", async () => {
    const root = new Context();
    root.provide("foo", 1);
    // provided by another plugin: an ancestor's own provisions need no declaration
    await root.plugin((ctx: Context) => void ctx.provide("bar", 2)).settled();
    const errors: unknown[] = [];
    await root
      .inject(["foo"], (ctx) => {
        expect(ctx.get("foo")).toBe(1);
        try {
          (ctx as Context).get("bar");
        } catch (error) {
          errors.push(error);
        }
        // reflective access is unchecked
        expect(ctx.live("bar")).toBe(2);
      })
      .settled();
    expect(errors).toEqual([new AccessError("undeclared", "bar")]);
  });

  it("let child plugins use what their parent injected", async () => {
    const root = new Context();
    root.provide("foo", 1);
    const seen: unknown[] = [];
    await root
      .inject(["foo"], async (ctx) => {
        await ctx.plugin((child: Context) => void seen.push(child.get("foo"))).settled();
      })
      .settled();
    expect(seen).toEqual([1]);
  });

  it("let the root read the live store unchecked", () => {
    const root = new Context();
    expect(() => root.get("foo")).toThrow("inactive context");
    expect(root.live("foo")).toBeUndefined();
    root.provide("foo", 1);
    expect(root.get("foo")).toBe(1);
  });

  it("give a Service's methods the accessing context as caller", async () => {
    const root = new Context();
    root.provide("counter", new (class extends Counter {})(root));
    const counter = root.get("counter") as Counter;
    void counter.increase();
    expect(counter.value).toBe(1);

    const fiber = root.inject(["counter"], (ctx) => {
      const view = ctx.get("counter") as Counter;
      expect(view.caller).toBe(ctx);
      expect(ctx.get("counter")).toBe(view);
      void view.increase();
      expect(view.value).toBe(2);
    });
    await fiber.settled();
    await fiber.dispose();
    expect(counter.value).toBe(1);
  });

  it("honor an availability check with the consumer's metadata", async () => {
    const root = new Context();
    root.provide("foo", {}, { check: (meta) => !meta.some((value) => (value as { deny?: boolean })?.deny) });
    const allowed = root.inject(["foo"], () => {});
    const denied = root.inject({ foo: { deny: true } }, () => {});
    await sleep();
    expect(allowed.state).toBe("active");
    expect(denied.state).toBe("pending");
  });
});

describe("Service disposal", () => {
  function fixture() {
    const root = new Context();
    const timeline: string[] = [];
    let closed = false;
    class Provider extends Service {
      constructor(ctx: Context) {
        super(ctx, "foo");
        closed = false;
        ctx.onDispose(() => {
          closed = true;
          timeline.push("provider closed");
        });
      }

      read() {
        timeline.push(`consumer read: ${closed ? "closed" : "open"}`);
      }
    }
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    return { root, Provider, timeline, gate, release, isClosed: () => closed };
  }

  it.each(["provider", "root", "provider first", "consumer first", "restart"])(
    "keeps a provider until its consumers have cleaned up (%s)",
    async (action) => {
      const { root, Provider, timeline, gate, release, isClosed } = fixture();
      const provider = root.plugin(Provider);
      await provider.settled();
      const consumer = root.inject(["foo"], (ctx) => {
        ctx.onDispose(async () => {
          await gate;
          (ctx.get("foo") as InstanceType<typeof Provider>).read();
        });
      });
      await consumer.settled();
      const task: Promise<unknown> =
        action === "root"
          ? root.fiber.dispose()
          : action === "restart"
            ? provider.restart()
            : action === "consumer first"
              ? Promise.all([consumer.dispose(), provider.dispose()])
              : action === "provider first"
                ? Promise.all([provider.dispose(), consumer.dispose()])
                : provider.dispose();
      await sleep();
      const closedDuringDrain = isClosed();
      release();
      await task;
      expect(closedDuringDrain).toBe(false);
      expect(timeline).toEqual(["consumer read: open", "provider closed"]);
      if (action === "restart") {
        await consumer.settled();
        expect(consumer.ctx.get("foo")).toBeDefined();
      }
    },
  );

  it("keeps transitive dependencies while a consumer that provides another service drains", async () => {
    const { root, Provider, timeline, gate, release, isClosed } = fixture();
    const fibers: Fiber[] = [];
    const owner = root.plugin((ctx: Context) => {
      fibers.push(ctx.plugin(Provider));
      fibers.push(
        ctx.inject(["foo"], (ctx) => {
          ctx.provide("bar", {});
          return () => (ctx.get("foo") as InstanceType<typeof Provider>).read();
        }),
      );
      fibers.push(
        ctx.inject(["bar"], () => async () => {
          timeline.push("consumer draining");
          await gate;
          timeline.push("consumer finished");
        }),
      );
    });
    await owner.settled();
    await Promise.all(fibers.map((fiber) => fiber.settled()));
    const task = owner.dispose();
    await sleep();
    const closedDuringDrain = isClosed();
    release();
    await task;
    expect(closedDuringDrain).toBe(false);
    expect(timeline).toEqual(["consumer draining", "consumer finished", "consumer read: open", "provider closed"]);
  });

  it("lets a successor provide while the retired provider's consumers drain", async () => {
    const root = new Context();
    let release!: () => void;
    const gate = new Promise<void>((resolve) => (release = resolve));
    const old = root.plugin((ctx: Context) => void ctx.provide("foo", "old"));
    await old.settled();
    const values: unknown[] = [];
    const consumer = root.inject(["foo"], (ctx) => {
      values.push(ctx.get("foo"));
      return async () => {
        await gate;
        values.push(`cleanup saw ${ctx.get("foo")}`);
      };
    });
    await consumer.settled();
    const disposing = old.dispose();
    // the realm is free at once
    root.plugin((ctx: Context) => void ctx.provide("foo", "new"));
    release();
    await disposing;
    await sleep();
    await consumer.settled();
    expect(values).toEqual(["old", "cleanup saw old", "new"]);
  });

  it("releases dependencies after a cleanup error", async () => {
    const { root, Provider, timeline } = fixture();
    const logs = captureLogs(root);
    await root.plugin(Provider).settled();
    await root
      .inject(["foo"], (ctx) => () => {
        (ctx.get("foo") as InstanceType<typeof Provider>).read();
        throw new Error("cleanup failed");
      })
      .settled();
    await root.fiber.dispose();
    expect(timeline).toEqual(["consumer read: open", "provider closed"]);
    expect(logs.errors()).toHaveLength(1);
  });
});
