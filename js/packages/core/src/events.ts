import type { Context } from "./context.js";
import { type EffectHandle, isPromiseLike } from "./effect.js";
import type { Fiber } from "./fiber.js";
import type { Root } from "./root.js";
import type { EventFilter, FiberState, ListenerEntry } from "./state.js";

/**
 * Event signatures by name. The runtime's own events are below; add yours by
 * declaration merging:
 *
 * ```ts
 * declare module "@kardiajs/core" {
 *   interface Events { "agent/status"(status: AgentStatus): void }
 * }
 * ```
 */
export interface Events {
  /** A fiber was created. */
  "internal/plugin-inserted"(fiber: Fiber): void;
  /** A fiber was retired. */
  "internal/plugin-removed"(fiber: Fiber): void;
  "internal/status"(fiber: Fiber, old: FiberState, state: FiberState): void;
  /** A binding of `key` appeared or disappeared in the listener's realm. */
  "internal/service"(key: string): void;
  /** A fiber's configuration was replaced through `update` (the write-back point). */
  "internal/commit"(fiber: Fiber, config: unknown): void;
  /** A fiber now runs another implementation (`registry.replace`). */
  "internal/replace"(fiber: Fiber): void;
}

export type EventName = keyof Events | (string & {});

export type Listener<K extends string> = K extends keyof Events ? Events[K] : (...args: any[]) => unknown;

export type EventArgs<K extends string> = Parameters<Listener<K>>;

/** A listener result that stops `bail` and `serial`: anything but `undefined`, `null` and `false`. */
export function isBailed(value: unknown) {
  return value !== undefined && value !== null && value !== false;
}

export function listen(ctx: Context, name: string, callback: (...args: any[]) => unknown, prepend: boolean): EffectHandle {
  const root = ctx._root;
  return ctx.effect((scope) => {
    const entry: ListenerEntry = {
      id: root.state.next("listener"),
      fiber: ctx._fiber,
      scope: ctx._scope,
      removed: false,
      callback,
    };
    const list = root.state.listeners.get(name) ?? [];
    if (prepend) list.unshift(entry);
    else list.push(entry);
    root.state.listeners.set(name, list);
    scope.defer(() => {
      entry.removed = true;
      const current = root.state.listeners.get(name);
      if (!current) return;
      const rest = current.filter((other) => other !== entry);
      if (rest.length) root.state.listeners.set(name, rest);
      else root.state.listeners.delete(name);
    });
  }, `ctx.on(${JSON.stringify(name)})`);
}

function listeners(root: Root, name: string, filter?: EventFilter) {
  return (root.state.listeners.get(name) ?? []).filter((entry) => !filter || filter(root.state, entry.scope));
}

function call(root: Root, entry: ListenerEntry, args: unknown[]): unknown {
  if (entry.removed) return;
  try {
    const result = entry.callback(...args);
    if (isPromiseLike(result)) {
      Promise.resolve(result).catch((error) => root.logInternal(entry.fiber, error));
      return;
    }
    return result;
  } catch (error) {
    root.logInternal(entry.fiber, error);
  }
}

/** Calls every listener; a failing listener is logged and the rest still run. Asynchronous listeners are not awaited. */
export function dispatch(root: Root, name: string, args: unknown[], filter?: EventFilter) {
  for (const entry of listeners(root, name, filter)) call(root, entry, args);
}

/** Returns the first bailed result of a synchronous listener. */
export function bail(root: Root, name: string, args: unknown[]): unknown {
  for (const entry of listeners(root, name)) {
    const result = call(root, entry, args);
    if (isBailed(result)) return result;
  }
}

/** Awaits listeners one at a time and returns the first bailed result. A failure rejects. */
export async function serial(root: Root, name: string, args: unknown[]): Promise<unknown> {
  for (const entry of listeners(root, name)) {
    if (entry.removed) continue;
    const result = await entry.callback(...args);
    if (isBailed(result)) return result;
  }
}

/** Awaits every listener concurrently; failures reject together as an `AggregateError`. */
export async function parallel(root: Root, name: string, args: unknown[]): Promise<void> {
  const results = await Promise.allSettled(
    listeners(root, name).map(async (entry) => {
      if (!entry.removed) await entry.callback(...args);
    }),
  );
  const errors = results.flatMap((result) => (result.status === "rejected" ? [result.reason] : []));
  if (errors.length) throw new AggregateError(errors, `one or more listeners of event "${name}" failed`);
}
