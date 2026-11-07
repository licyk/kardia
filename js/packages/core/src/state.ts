import { KardiaError } from "#/errors";
import type { EffectCell } from "#/effect";
import type { Level } from "#/logger";
import type { PluginDef } from "#/plugin";
import type { Root } from "#/root";
import { type FiberId, type Realm, ROOT_FIBER, Scope } from "#/scope";

/** The paper's θ. */
export type FiberState = "pending" | "loading" | "active" | "unloading" | "failed" | "disposed";

export type ImplId = number;

/** What a fiber should run against: ⊥ (`null`), or its revision plus the binding chosen for every declared key. */
export type Target = null | { revision: number; providers: ImplId[] };

export function sameTarget(a: Target, b: Target) {
  if (a === null || b === null) return a === b;
  return a.revision === b.revision && a.providers.length === b.providers.length && a.providers.every((id, i) => id === b.providers[i]);
}

/** An availability predicate of a binding, given the consumer's interception metadata (declared `inject` metadata first). */
export type Check = (meta: unknown[]) => boolean;

export interface FiberData {
  readonly id: FiberId;
  readonly parent: FiberId;
  readonly scope: Scope;
  runtime: object | undefined;
  /** The plugin's name, kept after the runtime is gone so a retiring fiber is still named. */
  name: string | undefined;
  inject: ReadonlyMap<string, unknown>;
  /** The source configuration, as given (not decoded). */
  config: unknown;
  state: FiberState;
  removed: boolean;
  revision: number;
  /** Live resolution of each declared key. */
  resolved: Map<string, ImplId>;
  target: Target;
  /** The target the current episode was loaded for. */
  episode: Target | undefined;
  /** The committed view ω, fixed for an episode. */
  committed: Map<string, ImplId> | undefined;
  /** A reload or unload transition is in flight. */
  inertia: boolean;
  error: Error | undefined;
  /** The fiber's own bindings, in provision order. */
  provisions: ImplId[];
  /** Set while the accumulator is being disposed during an unload. */
  draining: boolean;
  /** Provisions withdrawn during a drain; descendants still reach them until it ends. */
  withdrawn: ImplId[];
  /** The accumulator: effects in insertion order. */
  disposables: Map<number, EffectCell>;
  updateHooks: Map<number, UpdateHook>;
  /** The effect on the parent that instantiated this fiber. */
  retire: EffectCell | undefined;
}

export interface Impl {
  readonly key: string;
  realm: Realm;
  readonly provider: FiberId;
  readonly providerScope: Scope;
  readonly value: unknown;
  readonly check: Check | undefined;
  /** Fibers whose committed view names this binding. */
  readonly dependents: Set<FiberId>;
}

export interface RuntimeData {
  readonly def: PluginDef;
  readonly fibers: FiberId[];
}

export interface ListenerEntry {
  readonly id: number;
  readonly fiber: FiberId;
  readonly scope: Scope;
  removed: boolean;
  readonly callback: (...args: any[]) => unknown;
}

export interface UpdateHook {
  removed: boolean;
  readonly accept: (config: unknown) => boolean;
}

/** Whether a listener registered in `listener` receives an internal event. */
export type EventFilter = (st: State, listener: Scope) => boolean;

export type Action =
  | { type: "reload"; fiber: FiberId }
  | { type: "unload"; fiber: FiberId }
  | { type: "emit"; name: string; args: (root: Root) => unknown[]; filter?: EventFilter }
  | { type: "log"; fiber: FiberId; level: Level; error: unknown };

/**
 * All mutable runtime state of one root. Every synchronous segment of the
 * reference implementation is one critical section on it (see `Root.run`):
 * no user code runs inside one; follow-up work is queued as actions.
 */
export class State {
  readonly fibers = new Map<FiberId, FiberData>();
  /** Plugins with at least one fiber, in registration order. */
  readonly runtimes = new Map<object, RuntimeData>();
  /** Replaced plugins, forwarded to their replacement. */
  readonly aliases = new Map<object, PluginDef>();
  /** Every binding that still exists, live or withdrawn-but-referenced. */
  readonly impls = new Map<ImplId, Impl>();
  /** The live binding of each realm. */
  readonly store = new Map<Realm, ImplId>();
  /** Root realm of each key, assigned lazily on first provision. */
  readonly rootRealms = new Map<string, Realm>();
  readonly listeners = new Map<string, ListenerEntry[]>();
  actions: Action[] = [];
  private counters = { fiber: 0, impl: 0, entry: 0, listener: 0, hook: 0 };

  constructor() {
    const root = newFiber(ROOT_FIBER, ROOT_FIBER, Scope.root(), undefined, new Map(), undefined);
    root.state = "active";
    root.target = { revision: 0, providers: [] };
    root.episode = root.target;
    root.committed = new Map();
    this.fibers.set(ROOT_FIBER, root);
  }

  next(counter: keyof State["counters"]) {
    return ++this.counters[counter];
  }

  fiber(id: FiberId) {
    return this.fibers.get(id);
  }

  defOf(id: FiberId) {
    const runtime = this.fiber(id)?.runtime;
    return runtime && this.runtimes.get(runtime)?.def;
  }

  /** The plugin name of the fiber or its nearest named ancestor. */
  fiberName(id: FiberId): string {
    for (let fiber = this.fiber(id); fiber; fiber = this.fiber(fiber.parent)) {
      if (fiber.name) return fiber.name;
      if (fiber.id === ROOT_FIBER) break;
    }
    return "root";
  }

  /**
   * A retired fiber accepts no new effects, except from its own load still in
   * flight: those are collected and reverted by the unload that follows.
   */
  assertActive(id: FiberId) {
    const fiber = this.fiber(id);
    if (!fiber || (fiber.removed && fiber.state !== "loading")) throw KardiaError.inactiveEffect();
  }

  attachEffect(id: FiberId, cell: EffectCell) {
    const fiber = this.fiber(id);
    if (!fiber) return;
    const entry = this.next("entry");
    fiber.disposables.set(entry, cell);
    cell.entry = entry;
  }

  detachEffect(id: FiberId, entry: number) {
    this.fiber(id)?.disposables.delete(entry);
  }

  realmOf(scope: Scope, key: string): Realm | undefined {
    return scope.ownRealm(key) ?? this.rootRealms.get(key);
  }

  emit(name: string, args: (root: Root) => unknown[], filter?: EventFilter) {
    this.actions.push({ type: "emit", name, args, filter });
  }

  log(fiber: FiberId, level: Level, error: unknown) {
    this.actions.push({ type: "log", fiber, level, error });
  }
}

export function newFiber(
  id: FiberId,
  parent: FiberId,
  scope: Scope,
  def: PluginDef | undefined,
  inject: ReadonlyMap<string, unknown>,
  config: unknown,
): FiberData {
  return {
    id,
    parent,
    scope,
    runtime: def?.key,
    name: def?.name,
    inject,
    config,
    state: "pending",
    removed: false,
    revision: 0,
    resolved: new Map(),
    target: null,
    episode: undefined,
    committed: undefined,
    inertia: false,
    error: undefined,
    provisions: [],
    draining: false,
    withdrawn: [],
    disposables: new Map(),
    updateHooks: new Map(),
    retire: undefined,
  };
}

/** The state a fiber settles in once no transition is in flight. */
export function settledState(fiber: FiberData): FiberState {
  if (fiber.removed) return "disposed";
  if (fiber.error) return "failed";
  if (fiber.target !== null) return "active";
  return "pending";
}
