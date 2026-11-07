import type { ConfigOf, Context, Fiber, FiberState, Plugin, ServiceOf } from "@kardiajs/core";
import { type App, type InjectionKey, inject, markRaw, onScopeDispose, provide, type ShallowRef, shallowRef } from "vue";

/** Where components find their kardia context. */
export const contextKey: InjectionKey<Context> = Symbol("kardia context");

export interface Kardia {
  readonly context: Context;
  install(app: App): void;
}

/**
 * The Vue plugin: `app.use(createKardia(root))` makes `root` the context of
 * every component. Components rendered by an `<ExtensionSlot>` get the
 * context of the plugin that contributed them instead.
 */
export function createKardia(root: Context): Kardia {
  return {
    context: root,
    install(app) {
      app.provide(contextKey, markRaw(root));
    },
  };
}

/** The context of the current component: the app's root, or the contributing plugin's. */
export function useContext(): Context {
  const ctx = inject(contextKey, null);
  if (!ctx) throw new Error("no kardia context: install it with app.use(createKardia(root))");
  return ctx;
}

/** Makes `ctx` the context of the current component's subtree. */
export function provideContext(ctx: Context) {
  provide(contextKey, markRaw(ctx));
}

/**
 * Runs a plugin while the current component (or effect scope) lives: it is a
 * child of the component's context, and is disposed with the component.
 */
export function usePlugin<P extends Plugin>(plugin: P, config?: ConfigOf<P>): Fiber {
  const fiber = useContext().plugin(plugin, config);
  onScopeDispose(() => void fiber.dispose());
  return fiber;
}

/**
 * A service of the live store, as a ref that follows it: `undefined` while
 * no active provider offers it. Unlike `ctx.get`, it needs no declaration,
 * since a component is not a plugin.
 */
export function useService<K extends string>(key: K): Readonly<ShallowRef<ServiceOf<K> | undefined>> {
  const ctx = useContext();
  // annotated: shallowRef cannot infer through the generic `ServiceOf<K>`
  const service = shallowRef(ctx.live(key)) as ShallowRef<ServiceOf<K> | undefined>;
  const listener = ctx.on("internal/service", (changed) => {
    if (changed === key) service.value = ctx.live(key);
  });
  onScopeDispose(() => void listener.dispose());
  return service;
}

/** A fiber's state, as a ref that follows it. */
export function useFiberState(fiber: Fiber): Readonly<ShallowRef<FiberState>> {
  const state = shallowRef(fiber.state);
  const listener = fiber.ctx.root.on("internal/status", (target, _old, next) => {
    if (target.id === fiber.id) state.value = next;
  });
  onScopeDispose(() => void listener.dispose());
  return state;
}
