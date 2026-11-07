import type { Context } from "#/context";
import { type Disposer, EffectCell } from "#/effect";
import { type Fiber, fiberHandle } from "#/fiber";
import { checkImpl, collectRemoved, refresh, setTarget } from "#/lifecycle";
import { Level } from "#/logger";
import { type Plugin, type PluginDef, resolvePlugin } from "#/plugin";
import type { Root } from "#/root";
import type { FiberId } from "#/scope";
import { unbind } from "#/service";
import { newFiber, type State } from "#/state";

/** Registry operations of a root: the plugins with live fibers, and hot replacement. */
export class Registry {
  /** @internal */
  constructor(private readonly root: Root) {}

  /** The number of plugins with live fibers. */
  get size() {
    return this.root.state.runtimes.size;
  }

  /** The plugins with live fibers, in registration order. */
  keys(): object[] {
    return [...this.root.state.runtimes.keys()];
  }

  has(plugin: Plugin) {
    return this.root.with((st) => st.runtimes.has(forward(st, resolvePlugin(plugin)).key));
  }

  /** The live fibers of a plugin (following replacements). */
  fibers(plugin: Plugin): Fiber[] {
    return this.root.with((st) => {
      const runtime = st.runtimes.get(forward(st, resolvePlugin(plugin)).key);
      return (runtime?.fibers ?? []).map((id) => fiberHandle(this.root, id, st.fiber(id)!.scope));
    });
  }

  /**
   * Replaces a plugin's implementation in place: every fiber of `oldPlugin`
   * restarts running `newPlugin` with its own configuration, and later
   * instantiations of `oldPlugin` run `newPlugin`. Returns the moved fibers.
   */
  replace(oldPlugin: Plugin, newPlugin: Plugin): Fiber[] {
    const [oldDef, newDef] = [resolvePlugin(oldPlugin), resolvePlugin(newPlugin)];
    return this.root.run((st) => replace(st, oldDef, newDef)).map((id) =>
      fiberHandle(this.root, id, this.root.state.fiber(id)!.scope),
    );
  }
}

/** Follows replacements to the current definition. */
export function forward(st: State, def: PluginDef): PluginDef {
  for (let next = st.aliases.get(def.key); next; next = st.aliases.get(def.key)) def = next;
  return def;
}

export function insertFiber(root: Root, st: State, parent: Context, plugin: Plugin, config: unknown): Fiber {
  st.assertActive(parent._fiber);
  const def = forward(st, resolvePlugin(plugin));
  if (!st.runtimes.has(def.key)) st.runtimes.set(def.key, { def, fibers: [] });
  const id = st.next("fiber");
  const scope = parent._scope.child({ type: "fiber", fiber: id });
  st.fibers.set(id, newFiber(id, parent._fiber, scope, def, def.inject, config));
  st.emit("internal/plugin-inserted", (root) => [fiberHandle(root, id, scope)]);
  for (const key of def.inject.keys()) checkImpl(st, id, key);
  st.runtimes.get(def.key)!.fibers.push(id);
  refresh(st, id);
  const cell = new EffectCell("ctx.plugin()", parent._fiber);
  cell.pushDisposer(retire(root, id), (error) => root.logInternal(parent._fiber, error));
  st.attachEffect(parent._fiber, cell);
  st.fiber(id)!.retire = cell;
  warnCycle(st, id);
  return fiberHandle(root, id, scope);
}

/** The synchronous part of retiring a fiber. */
function retireFiber(st: State, id: FiberId) {
  const fiber = st.fiber(id);
  if (!fiber || fiber.removed) return;
  fiber.removed = true;
  const scope = fiber.scope;
  const name = st.fiberName(id);
  st.emit("internal/plugin-removed", (root) => [fiberHandle(root, id, scope, name)]);
  const runtime = fiber.runtime && st.runtimes.get(fiber.runtime);
  if (runtime) {
    runtime.fibers.splice(runtime.fibers.indexOf(id), 1);
    if (!runtime.fibers.length) st.runtimes.delete(fiber.runtime!);
  }
  // A retired provider's bindings stop occupying their realms at once, so a
  // successor can provide there while old consumers still drain (they read
  // through their committed views).
  for (const implId of [...fiber.provisions]) unbind(st, implId);
  setTarget(st, id, null);
  collectRemoved(st, id);
}

/** The inverse of an instantiation: retire the fiber, then wait for its transitions to settle. */
function retire(root: Root, id: FiberId): Disposer {
  return () => {
    root.run((st) => retireFiber(st, id));
    return root
      .waitUntil((st) => !st.fiber(id)?.inertia)
      .then(() => root.run((st) => collectRemoved(st, id)));
  };
}

function replace(st: State, oldDef: PluginDef, newDef: PluginDef): FiberId[] {
  const oldKey = forward(st, oldDef).key;
  const key = newDef.key;
  // `newDef` is an implementation of its own again
  st.aliases.delete(key);
  const fibers = [...(st.runtimes.get(oldKey)?.fibers ?? [])];
  if (oldKey !== key) {
    st.runtimes.delete(oldKey);
    st.aliases.set(oldKey, newDef);
  }
  if (!fibers.length) return fibers;
  if (!st.runtimes.has(key)) st.runtimes.set(key, { def: newDef, fibers: [] });
  for (const id of fibers) replaceFiber(st, id, newDef);
  return fibers;
}

function replaceFiber(st: State, id: FiberId, def: PluginDef) {
  const fiber = st.fiber(id)!;
  if (fiber.runtime !== def.key) {
    fiber.runtime = def.key;
    st.runtimes.get(def.key)!.fibers.push(id);
  }
  fiber.name = def.name;
  fiber.inject = def.inject;
  fiber.resolved.clear();
  const scope = fiber.scope;
  st.emit("internal/replace", (root) => [fiberHandle(root, id, scope)]);
  for (const key of def.inject.keys()) checkImpl(st, id, key);
  fiber.error = undefined;
  fiber.revision++;
  refresh(st, id);
}

/**
 * Warns when the declared provisions and dependencies of live fibers form a
 * cycle through the new fiber; such a fiber stays pending.
 */
function warnCycle(st: State, start: FiberId) {
  const path = [start];
  if (findCycle(st, start, start, path, new Set())) {
    const names = path.map((id) => st.fiberName(id));
    st.log(start, Level.warn, new Error(`dependency cycle: ${names.join(" -> ")}`));
  }
}

function findCycle(st: State, start: FiberId, at: FiberId, path: FiberId[], visited: Set<FiberId>): boolean {
  const consumer = st.fiber(at)!;
  for (const key of consumer.inject.keys()) {
    const realm = st.realmOf(consumer.scope, key);
    for (const runtime of st.runtimes.values()) {
      if (!runtime.def.provide.includes(key)) continue;
      for (const provider of runtime.fibers) {
        const data = st.fiber(provider);
        if (!data || data.removed || st.realmOf(data.scope, key) !== realm) continue;
        if (provider === start) {
          path.push(start);
          return true;
        }
        if (visited.has(provider)) continue;
        visited.add(provider);
        path.push(provider);
        if (findCycle(st, start, provider, path, visited)) return true;
        path.pop();
      }
    }
  }
  return false;
}

