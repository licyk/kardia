import { Context, type EffectHandle } from "@kardiajs/core";
import { describe, expect, it, vi } from "vitest";
import { createApp, createSSRApp, defineComponent, effectScope, h, nextTick, watch } from "vue";
import { renderToString } from "vue/server-renderer";

import {
  addRoute,
  type ComponentContribution,
  type Contribution,
  Contributions,
  createKardia,
  defineContributions,
  ExtensionSlot,
  useContext,
  useContributions,
  useFiberState,
  usePlugin,
  useService,
} from "#/index";

interface View extends Contribution {
  title: string;
}

declare module "@kardiajs/core" {
  interface Services {
    "ui.views": Contributions<View>;
    "ui.panels": Contributions<ComponentContribution>;
  }
}

/** Runs `setup` in an effect scope of an app that has `root` installed, as a component's setup would. */
function inApp<T>(root: Context, setup: () => T) {
  const app = createApp({ render: () => null });
  app.use(createKardia(root));
  const scope = effectScope();
  const value = app.runWithContext(() => scope.run(setup)!);
  return { value, stop: () => scope.stop() };
}

const listeners = (root: Context, name: string) => root._root.state.listeners.get(name)?.length ?? 0;

describe("Vue integration", () => {
  it("gives components the app's context, and fails clearly without it", () => {
    const root = new Context();
    expect(inApp(root, useContext).value).toBe(root);
    expect(() => effectScope().run(useContext)).toThrow("app.use(createKardia(root))");
  });

  it("runs a plugin while the component lives", async () => {
    const root = new Context();
    const dispose = vi.fn();
    const { value: fiber, stop } = inApp(root, () => usePlugin(() => dispose));
    await fiber.settled();
    expect(fiber.state).toBe("active");
    stop();
    await fiber.settled();
    expect(fiber.state).toBe("disposed");
    expect(dispose).toHaveBeenCalledTimes(1);
  });

  it("follows a service's availability, and stops following with the component", async () => {
    const root = new Context();
    const { value: service, stop } = inApp(root, () => useService("store"));
    expect(service.value).toBeUndefined();
    const provider = root.plugin((ctx: Context) => void ctx.provide("store", { ready: true }));
    await provider.settled();
    expect(service.value).toEqual({ ready: true });
    await provider.dispose();
    expect(service.value).toBeUndefined();
    expect(listeners(root, "internal/service")).toBe(1);
    stop();
    expect(listeners(root, "internal/service")).toBe(0);
  });

  it("follows a fiber's state", async () => {
    const root = new Context();
    const fiber = root.inject(["store"], () => {});
    const { value: state, stop } = inApp(root, () => useFiberState(fiber));
    expect(state.value).toBe("pending");
    root.provide("store", {});
    await fiber.settled();
    expect(state.value).toBe("active");
    stop();
  });
});

describe("Contributions", () => {
  async function setup() {
    const root = new Context();
    await root.plugin(defineContributions<View>("ui.views")).settled();
    return root;
  }

  it("orders items, and removes them with the plugin that contributed them", async () => {
    const root = await setup();
    const registry = root.get("ui.views");
    const extension = root.inject(["ui.views"], (ctx) => {
      ctx.get("ui.views").register({ id: "b", title: "B", order: 2 });
      ctx.get("ui.views").register({ id: "a", title: "A", order: 1 });
      ctx.get("ui.views").register({ id: "c", title: "C", order: 2 });
    });
    await extension.settled();
    expect(registry.items.map((item) => item.id)).toEqual(["a", "b", "c"]);
    expect(registry.entries.map((entry) => entry.owner.fiber.id)).toEqual([extension.id, extension.id, extension.id]);

    await extension.dispose();
    expect(registry.items).toEqual([]);
  });

  it("refuses a taken id", async () => {
    const root = await setup();
    root.get("ui.views").register({ id: "a", title: "A" });
    expect(() => root.get("ui.views").register({ id: "a", title: "again" })).toThrow('ui.views already has a contribution "a"');
  });

  it("is reactive", async () => {
    const root = await setup();
    const { value: items, stop } = inApp(root, () => useContributions<View>("ui.views"));
    const seen: string[][] = [];
    const scope = effectScope();
    scope.run(() => watch(items, (list) => void seen.push(list.map((item) => item.title))));
    const handle: EffectHandle = root.get("ui.views").register({ title: "A" });
    await nextTick();
    await handle.dispose();
    await nextTick();
    expect(seen).toEqual([["A"], []]);
    scope.stop();
    stop();
  });

  it("holds an extension's view exactly while the host service it depends on is there", async () => {
    const root = await setup();
    const host = root.plugin((ctx: Context) => void ctx.provide("host.web", {}));
    root.inject(["ui.views", "host.web"], (ctx) => void ctx.get("ui.views").register({ id: "web", title: "Web search" }));
    await host.settled();
    await vi.waitFor(() => expect(root.get("ui.views").items.map((item) => item.id)).toEqual(["web"]));
    await host.dispose();
    await vi.waitFor(() => expect(root.get("ui.views").items).toEqual([]));
  });

  it("adds routes that leave with their plugin", async () => {
    const root = new Context();
    const routes = new Set<string>();
    const router = { addRoute: (route: string) => (routes.add(route), () => void routes.delete(route)) };
    const extension = root.plugin((ctx: Context) => void addRoute(ctx, router, "/web"));
    await extension.settled();
    expect([...routes]).toEqual(["/web"]);
    await extension.dispose();
    expect([...routes]).toEqual([]);
  });
});

describe("ExtensionSlot", () => {
  const Greeting = defineComponent({
    props: { greeting: String, name: String },
    setup: (props) => () => h("p", `${props.greeting}, ${props.name}`),
  });

  /** Renders its owner: the context `<ExtensionSlot>` gives a contributed component. */
  const Owner = defineComponent({
    setup() {
      const ctx = useContext();
      return () => h("p", `owned by ${ctx.fiber.name}`);
    },
  });

  async function render(root: Context, slots: Record<string, unknown> = {}) {
    const app = createSSRApp({ render: () => h(ExtensionSlot, { name: "ui.panels", props: { greeting: "hi" } }, slots) });
    app.use(createKardia(root));
    // without the server renderer's fragment markers
    return (await renderToString(app)).replace(/<!--[[\]]-->/g, "");
  }

  async function setup() {
    const root = new Context();
    await root.plugin(defineContributions<ComponentContribution>("ui.panels")).settled();
    return root;
  }

  it("renders the contributions in order, each with its owner's context and merged props", async () => {
    const root = await setup();
    await root
      .plugin({
        name: "web-panels",
        inject: ["ui.panels"],
        apply(ctx: Context<"ui.panels">) {
          ctx.get("ui.panels").register({ id: "owner", component: Owner, order: 2 });
          ctx.get("ui.panels").register({ id: "greeting", component: Greeting, props: { name: "web" }, order: 1 });
        },
      })
      .settled();
    // a component that does not declare a slot prop gets it as an attribute, as Vue does
    expect(await render(root)).toBe('<p>hi, web</p><p greeting="hi">owned by web-panels</p>');
  });

  it("renders its default slot while there is nothing to show, even without a registry", async () => {
    expect(await render(new Context(), { default: () => h("em", "nothing yet") })).toBe("<em>nothing yet</em>");
    expect(await render(await setup(), { default: () => h("em", "nothing yet") })).toBe("<em>nothing yet</em>");
  });

  it("lets the item slot render each contribution", async () => {
    const root = await setup();
    root.get("ui.panels").register({ id: "greeting", component: Greeting });
    const html = await render(root, { item: ({ item }: { item: ComponentContribution }) => h("li", item.id) });
    expect(html).toBe("<li>greeting</li>");
  });
});
