import type { EffectCell } from "./effect.js";
import { Interrupted, toError } from "./errors.js";
import { fiberHandle } from "./fiber.js";
import { Level } from "./logger.js";
import type { PluginDef } from "./plugin.js";
import type { Root } from "./root.js";
import type { FiberId, Realm, Scope } from "./scope.js";
import { removeProvision } from "./service.js";
import { type FiberState, type ImplId, type State, type Target, sameTarget, settledState } from "./state.js";

// The fiber state machine: a port of Kardia's `lifecycle.rs` (itself a port of
// Cordis' `_checkImpl` / `_refresh` / `_setEpoch` / `_reload` / `_unload` /
// `_updateState`), split into critical sections and the asynchronous
// transitions that run between them.

/** The binding a scope would see in the live store; `strict` also requires an active provider. */
export function liveImpl(st: State, scope: Scope, key: string, strict: boolean): ImplId | undefined {
  const realm = st.realmOf(scope, key);
  if (!realm) return;
  const id = st.store.get(realm);
  if (id === undefined) return;
  if (strict && st.fiber(st.impls.get(id)!.provider)?.state !== "active") return;
  return id;
}

/** Re-resolves one declared key of a fiber against the live store. */
export function checkImpl(st: State, id: FiberId, key: string) {
  const fiber = st.fiber(id);
  if (!fiber) return;
  let found = liveImpl(st, fiber.scope, key, true);
  const check = found === undefined ? undefined : st.impls.get(found)!.check;
  if (check) {
    const declared = fiber.inject.get(key);
    const meta = [...(declared === undefined ? [] : [declared]), ...fiber.scope.intercepts(key)];
    if (!check(meta)) found = undefined;
  }
  if (found === undefined) fiber.resolved.delete(key);
  else fiber.resolved.set(key, found);
}

function computeTarget(st: State, id: FiberId): Target {
  const fiber = st.fiber(id)!;
  const providers: ImplId[] = [];
  for (const key of fiber.inject.keys()) {
    const impl = fiber.resolved.get(key);
    if (impl === undefined) return null;
    providers.push(impl);
  }
  return { revision: fiber.revision, providers };
}

export function refresh(st: State, id: FiberId) {
  if (st.fibers.has(id)) setTarget(st, id, computeTarget(st, id));
}

/** Records the new target and starts a transition unless one is in flight or the fiber has failed. */
export function setTarget(st: State, id: FiberId, target: Target) {
  const fiber = st.fiber(id)!;
  if (sameTarget(fiber.target, target) || fiber.error) return;
  const old = fiber.target;
  fiber.target = target;
  if (fiber.inertia) return;
  if (target !== null && old === null) {
    beginReload(st, id);
    updateState(st, id, "loading");
  } else {
    beginUnload(st, id);
    updateState(st, id, "unloading");
  }
}

/** On entering or leaving `active`, consumers of the fiber's live provisions are notified. */
export function updateState(st: State, id: FiberId, next?: FiberState) {
  const fiber = st.fiber(id)!;
  const old = fiber.state;
  fiber.state = next ?? settledState(fiber);
  const state = fiber.state;
  if (old === state) return;
  const scope = fiber.scope;
  const name = st.fiberName(id);
  st.emit("internal/status", (root) => [fiberHandle(root, id, scope, name), old, state]);
  if (old !== "active" && state !== "active") return;
  for (const implId of [...fiber.provisions]) {
    const impl = st.impls.get(implId)!;
    if (st.store.get(impl.realm) === implId) notify(st, impl.key, impl.realm);
  }
}

function beginReload(st: State, id: FiberId) {
  const fiber = st.fiber(id)!;
  fiber.inertia = true;
  fiber.episode = fiber.target;
  const committed = new Map(fiber.resolved);
  for (const implId of committed.values()) {
    const impl = st.impls.get(implId);
    if (impl && impl.provider !== id) impl.dependents.add(id);
  }
  fiber.committed = committed;
  st.actions.push({ type: "reload", fiber: id });
}

function beginUnload(st: State, id: FiberId) {
  st.fiber(id)!.inertia = true;
  st.actions.push({ type: "unload", fiber: id });
}

/** Re-resolves every fiber that declares `key` in `realm`. */
export function notify(st: State, key: string, realm: Realm) {
  const ids = [...st.runtimes.values()].flatMap((runtime) => runtime.fibers);
  for (const id of ids) {
    const fiber = st.fiber(id);
    if (!fiber || !fiber.inject.has(key) || st.realmOf(fiber.scope, key) !== realm) continue;
    checkImpl(st, id, key);
    refresh(st, id);
  }
  st.emit(
    "internal/service",
    () => [key],
    (st, listener) => st.realmOf(listener, key) === realm,
  );
}

/** The load of `id` is still current and should run `apply`. */
function prepareExecute(st: State, id: FiberId): { def: PluginDef | undefined; config: unknown } | undefined {
  const fiber = st.fiber(id);
  if (!fiber || !fiber.episode || !sameTarget(fiber.episode, fiber.target)) return;
  return { def: st.defOf(id), config: fiber.config };
}

function finishReload(st: State, id: FiberId, error: unknown) {
  const fiber = st.fiber(id)!;
  if (error !== undefined && !(error instanceof Interrupted)) {
    st.log(id, Level.error, error);
    // a superseded load still needs cleanup, but cannot fail the new target
    if (fiber.episode && sameTarget(fiber.episode, fiber.target)) {
      fiber.error = toError(error);
      fiber.target = null;
    }
  }
  if (fiber.episode !== undefined && sameTarget(fiber.episode, fiber.target)) {
    fiber.inertia = false;
    updateState(st, id);
  } else {
    beginUnload(st, id);
    updateState(st, id, "unloading");
  }
}

function startDrain(st: State, id: FiberId): EffectCell[] {
  const fiber = st.fiber(id)!;
  fiber.draining = true;
  const cells = [...fiber.disposables.values()].reverse();
  fiber.disposables.clear();
  for (const cell of cells) cell.entry = undefined;
  return cells;
}

function finishUnload(st: State, id: FiberId) {
  const fiber = st.fiber(id)!;
  fiber.draining = false;
  fiber.episode = undefined;
  const withdrawn = fiber.withdrawn.splice(0);
  const committed = fiber.committed ?? new Map<string, ImplId>();
  fiber.committed = undefined;
  for (const implId of withdrawn) removeProvision(st, id, implId);
  // release the providers waiting for this fiber to unload
  for (const implId of committed.values()) st.impls.get(implId)?.dependents.delete(id);
  if (fiber.target === null) {
    fiber.inertia = false;
    updateState(st, id);
    collectRemoved(st, id);
  } else {
    beginReload(st, id);
    updateState(st, id, "loading");
  }
}

/** Settles a removed fiber with no transition in flight to `disposed` and drops it. */
export function collectRemoved(st: State, id: FiberId) {
  const fiber = st.fiber(id);
  if (!fiber || !fiber.removed || fiber.inertia) return;
  updateState(st, id);
  st.fibers.delete(id);
}

/** `_reload` after its synchronous prefix. */
export async function reload(root: Root, id: FiberId) {
  // let superseding updates of the same tick coalesce
  await Promise.resolve();
  const job = root.with((st) => prepareExecute(st, id));
  let error: unknown;
  if (job?.def) {
    try {
      await execute(root, id, job.def, job.config);
    } catch (reason) {
      error = reason ?? new Error(String(reason));
    }
  }
  root.run((st) => finishReload(st, id, error));
}

async function execute(root: Root, id: FiberId, def: PluginDef, config: unknown) {
  const fiber = fiberHandle(root, id, root.state.fiber(id)!.scope);
  const result = await def.apply(fiber.ctx, def.decode(config));
  if (typeof result === "function") fiber.ctx.onDispose(result as () => void);
}

/** `_unload`: waits for the fiber's dependents, then disposes its effects newest first, concurrently. */
export async function unload(root: Root, id: FiberId) {
  // Dependents are notified when this fiber leaves `active`, but may still
  // access its services during their own teardown.
  await root.waitUntil((st) => {
    const fiber = st.fiber(id);
    return !fiber || fiber.provisions.every((implId) => !st.impls.get(implId)?.dependents.size);
  });
  const cells = root.run((st) => startDrain(st, id));
  const results = await Promise.allSettled(cells.map((cell) => cell.dispose(root)));
  for (const result of results) {
    if (result.status === "rejected") root.run((st) => st.log(id, Level.error, result.reason));
  }
  root.run((st) => finishUnload(st, id));
}
