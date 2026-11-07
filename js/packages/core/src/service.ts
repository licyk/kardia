import type { Context } from "#/context";
import type { Disposer } from "#/effect";
import { AccessError, KardiaError } from "#/errors";
import { liveImpl, notify } from "#/lifecycle";
import type { Root } from "#/root";
import { type FiberId, ROOT_FIBER } from "#/scope";
import type { Check, ImplId, State } from "#/state";

/**
 * Service types by key, for typed access. Extend it by declaration merging:
 *
 * ```ts
 * declare module "@kardiajs/core" {
 *   interface Services { timer: Timer }
 * }
 * ```
 */
export interface Services {}

/** The type of the service under `K`: its declared type, or `unknown`. */
export type ServiceOf<K extends string> = K extends keyof Services ? Services[K] : unknown;

const bindSymbol = Symbol.for("kardia.bind");

/**
 * Base class for services that act on behalf of their caller, like Kardia's
 * `Svc::caller`: `ctx.get(key)` returns a view of the service whose
 * {@link Service.caller} is the accessing context, so effects created through
 * `this.caller` belong to the caller's fiber and stop with it.
 */
export abstract class Service {
  /** Implement `[Service.bind](caller)` to return the value `ctx.get` hands to `caller`. */
  static readonly bind: typeof bindSymbol = bindSymbol;

  /** The context that accessed this view of the service (the provider's own context on the instance). */
  declare readonly caller: Context;

  /** Provides the instance under `key` from `ctx` when `key` is given. */
  constructor(
    /** The context the service lives in: where it resolves its own dependencies. */
    protected readonly ctx: Context,
    key?: string,
  ) {
    Object.defineProperty(this, "caller", { value: ctx, configurable: true });
    if (key !== undefined) ctx.provide(key, this);
  }

  /**
   * The view handed to `caller`: the service itself, except that `caller`
   * reads as the accessing context. Writes go to the service. (Use `private`
   * rather than `#private` fields: those do not pass through the view.)
   */
  [bindSymbol](caller: Context): this {
    return new Proxy(this, {
      get: (target, prop, receiver) => (prop === "caller" ? caller : Reflect.get(target, prop, receiver)),
      set: (target, prop, value) => Reflect.set(target, prop, value),
    });
  }
}

export interface Bindable {
  [bindSymbol](caller: Context): unknown;
}

export function isBindable(value: unknown): value is Bindable {
  return (typeof value === "object" || typeof value === "function") && value !== null && bindSymbol in value;
}

type Located = { own: ImplId } | { fiber: FiberId; binding: ImplId | undefined };

/**
 * Walks from the context up through its fibers: own provisions in the same
 * realm, then the nearest fiber declaring the key. The walk stops at a
 * context that isolates the key again.
 */
export function locate(st: State, ctx: Context, key: string): Located | undefined {
  const realm = st.realmOf(ctx._scope, key);
  let node = ctx._scope as Context["_scope"] | undefined;
  let id = ctx._fiber;
  while (true) {
    const fiber = st.fiber(id);
    if (!fiber) return;
    const own = [...fiber.provisions].reverse().find((implId) => {
      const impl = st.impls.get(implId);
      return impl && impl.key === key && st.realmOf(impl.providerScope, key) === realm;
    });
    if (own !== undefined) return { own };
    while (true) {
      if (!node) return;
      if (node === fiber.scope) break;
      if (node.isolates(key)) return;
      node = node.parent;
    }
    const binding = fiber.committed?.get(key);
    if (binding !== undefined || fiber.inject.has(key)) return { fiber: id, binding };
    if (id === ROOT_FIBER) return;
    node = fiber.scope.parent;
    if (!node) return;
    id = node.fiber();
  }
}

/** The binding an access from `ctx` reaches. The root context has unchecked access to the live store. */
export function access(st: State, ctx: Context, key: string): ImplId {
  if (!st.fiber(ctx._fiber)) throw new AccessError("inactive", key);
  let found: ImplId | undefined;
  if (ctx._fiber === ROOT_FIBER) {
    found = liveImpl(st, ctx._scope, key, false);
  } else {
    const located = locate(st, ctx, key);
    if (!located) throw new AccessError("undeclared", key);
    found = "own" in located ? located.own : located.binding;
  }
  if (found === undefined || !st.impls.has(found)) throw new AccessError("inactive", key);
  return found;
}

/** Interception metadata the fiber whose `inject` admits the access declares for `key`. */
export function declaredMeta(st: State, ctx: Context, key: string): unknown {
  const located = locate(st, ctx, key);
  if (!located || "own" in located) return;
  return st.fiber(located.fiber)?.inject.get(key);
}

export function bind(st: State, ctx: Context, key: string, value: unknown, check: Check | undefined): ImplId {
  // a retired fiber's stale load may still collect inverses, but it must not
  // occupy a realm its successor may already be providing in
  const fiber = st.fiber(ctx._fiber);
  if (!fiber || fiber.removed) throw KardiaError.inactiveEffect();
  let rootRealm = st.rootRealms.get(key);
  if (!rootRealm) st.rootRealms.set(key, (rootRealm = Symbol(key)));
  const realm = ctx._scope.ownRealm(key) ?? rootRealm;
  const existing = st.store.get(realm);
  if (existing !== undefined) throw KardiaError.alreadyProvided(key, st.fiberName(st.impls.get(existing)!.provider));
  const id = st.next("impl");
  st.impls.set(id, {
    key,
    realm,
    provider: ctx._fiber,
    providerScope: ctx._scope,
    value,
    check,
    dependents: new Set(),
  });
  st.store.set(realm, id);
  fiber.provisions.push(id);
  if (fiber.state === "active") notify(st, key, realm);
  return id;
}

export function unbind(st: State, id: ImplId) {
  const impl = st.impls.get(id);
  if (!impl) return;
  if (st.store.get(impl.realm) === id) {
    st.store.delete(impl.realm);
    notify(st, impl.key, impl.realm);
  }
}

/**
 * Removes a withdrawn binding from its provider. During a drain this waits
 * for the drain's end, so the provider's descendants still reach it during
 * their own cleanup.
 */
function retireProvision(st: State, id: ImplId) {
  const impl = st.impls.get(id);
  if (!impl) return;
  const provider = st.fiber(impl.provider);
  if (provider?.draining) {
    provider.withdrawn.push(id);
    return;
  }
  removeProvision(st, impl.provider, id);
}

export function removeProvision(st: State, provider: FiberId, id: ImplId) {
  const fiber = st.fiber(provider);
  if (fiber) fiber.provisions = fiber.provisions.filter((p) => p !== id);
  st.impls.delete(id);
}

/** The inverse of a provision: leave the realm now, retire once the consumers that committed to it are gone. */
export function withdraw(root: Root, id: ImplId): Disposer {
  return () => {
    root.run((st) => unbind(st, id));
    return root
      .waitUntil((st) => !st.impls.get(id)?.dependents.size)
      .then(() => root.run((st) => retireProvision(st, id)));
  };
}
