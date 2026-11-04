# Cordis Core: Fibers, Registry and the Component Lifecycle

> **Layer:** Implementation (`cordis/packages/core/src/fiber.ts`, `registry.ts`).
> Paper counterpart: §4.1–4.2 (calculus) and §5.1.3 (Algorithms 4–5) – see [`../paper/05-calculus.md`](../paper/05-calculus.md) and [`../paper/07-implementation-in-paper.md`](../paper/07-implementation-in-paper.md).

## 1. Plugins (components) and runtimes (`registry.ts`)

A **plugin** (the paper's *component*) is one of:
```ts
type Plugin<T> =
  | ((ctx, config: T) => any)                    // function plugin
  | (new (ctx, config: T) => any)                // class plugin (typically a Service subclass)
  | { apply(ctx, config: T): any }               // object plugin
// with optional static metadata:
interface Base<T> { name?; Config?: StandardSchemaV1<any, T>; inject?: Inject; provide?: string | string[]; intercept?: Dict<boolean> }
```
* `inject` is either `['a', 'b']` or `{ a: meta, b: meta }` (values = component-declared interception metadata, Definition 26). `Inject.resolve` normalizes to `Dict<meta | null>`; the `@Inject` decorator (class or method) builds inherited inject tables (`registry.ts:17-61`).
* `provide` is declarative only (used by the loader / tooling); the actual provision happens when the plugin calls `ctx.provide` / constructs a `Service`.
* The *return value* of `apply` is treated as an effect (§[03-effects](03-effects.md)) – a function, a promise, or a (async) generator of disposers.

`RegistryService` keeps `Map<callback, Plugin.Runtime>`:
```ts
interface Runtime { name?: string; fibers: DisposableList<Fiber>; callback: Function; Config?: StandardSchemaV1 }
```
A runtime groups all fibers of one plugin; it is unregistered when its last fiber is removed (`fiber.ts:191-193`). `registry.counter` provides **fiber uids** (`++counter`, never reused).

### `ctx.plugin(plugin, config)` (`registry.ts:231-245`)
Resolve aliases (see `replace`), `assertActive()` on the calling fiber, get/create the runtime, `new Fiber(ctx, config, Inject.resolve(plugin.inject), runtime, outerStack)`, and return an object whose prototype is the fiber and which is **thenable** (`await ctx.plugin(...)` ≡ `fiber.await()`). `ctx.inject(deps, cb)` is sugar for `ctx.plugin({ inject: deps, apply: cb })`.

### `registry.replace(old, new)` (`registry.ts:185-209`)
Swaps the implementation behind existing fibers **in place**: records an alias `old → new` (so later `ctx.plugin(old)` instantiates `new`; aliases are followed in chains and can switch back), moves every fiber of the old runtime to the new runtime via `fiber._replace(runtime, inject)`. `_replace` (`fiber.ts:401-416`) re-checks injections against the new `inject`, clears `_error`, bumps `_revision`, and `_refresh()`es – the fiber keeps identity, parent and config but reloads with the new code. This is the primitive HMR uses (see [09-hmr](09-hmr.md)).

## 2. The `Fiber` data structure (`fiber.ts:115-220`)

| Field | Paper | Notes |
|---|---|---|
| `uid: number \| null` | name n | `null` once removed (O-Remove) |
| `parent: Context` | π | the context it was created on; parent fiber = `parent.fiber` |
| `ctx: Context` | derived context | `parent.extend({ fiber: this })` |
| `config` | bound into e | raw config; validated through `internal/config` on each load |
| `inject: Dict` | d | declared keys (+ metadata) |
| `runtime` | component | `null` for the root fiber |
| `state: FiberState` | θ (projection) | `PENDING, LOADING, ACTIVE, FAILED, DISPOSED, UNLOADING` |
| `provisions: Set<Impl>` | σ_n | bindings this fiber installed |
| `committed?: Dict<Impl>` | ω | snapshot of resolved impls at reload start |
| `_store: Dict<Impl>` | live resolution | per declared key: currently available impl (or missing) |
| `_runner.epoch: string` | target digest | `"<revision>:<uid₁>:<uid₂>…"` or `'__INACTIVE__'` (⊥) |
| `_revision` | – | bumped by `restart()`/`_replace()` to force a reload (configuration revision) |
| `inertia?: Promise` | Future | the transition in flight |
| `_disposables` | accumulator g | `DisposableList` of effect wrappers |
| `_error` | failure outcome | set when a load fails; blocks re-entry |
| `_release?` | guard waiter | resolver used while waiting for dependents |
| `_remove?` | runtime membership | removes the fiber from `runtime.fibers` |
| `_hooks` | – | per-fiber listener lists (`internal/update`) |
| `dispose()` | O-Retire | the parent effect's disposer |

### `FiberState` vs. paper θ
| Cordis | Paper |
|---|---|
| `PENDING` | `Inactive` with target ⊥ (waiting for dependencies) |
| `LOADING` | `Reloading` |
| `ACTIVE` | `Active` |
| `UNLOADING` | `Unloading` |
| `FAILED` | `Inactive` + error outcome (§4.4 Failure) |
| `DISPOSED` | removed from the registry (uid `null`) |

`_getState()` derives the settled state: `uid === null → DISPOSED`, `_error → FAILED`, `epoch !== INACTIVE → ACTIVE`, else `PENDING`.

## 3. Instantiation – O-Insert / O-Retire (`fiber.ts:148-206`)

```ts
this.uid = parent.registry.counter
this.ctx = parent.extend({ fiber: this })
emit 'internal/plugin'(this)
for name of inject: this._checkImpl(name)            // initial resolution
this.dispose = parent.fiber.effect(() => {           // ← tracked effect of the PARENT fiber
  this._remove = runtime.fibers.push(this)           //   O-Insert: enter the registry
  try { this._refresh() } catch (e) { log; this._error = e }
  return async () => {                               //   inverse = O-Retire (+ removal)
    this.uid = null; emit 'internal/plugin'(this)
    this._remove(); if (!runtime.fibers.length) registry._unregister(runtime)
    this._setEpoch(INACTIVE)                         //   target ← ⊥
    while (this.inertia) await this.inertia          //   wait for the transition chain
    this._updateState(() => {})                      //   settle → DISPOSED
  }
}, 'ctx.plugin()')
```
So a child fiber is exactly Definition 52: an ordinary effect of the parent whose inverse retires it. Unloading a parent cascades to children through the parent's accumulator. Unlike the calculus, retirement and removal are fused: the inverse retires, waits for the chain to finish, then the fiber is gone (uid already `null`).

## 4. Target computation – `_checkImpl` / `_refresh` (`fiber.ts:387-429`)

```ts
_checkImpl(name):                       // resolve one declared key
  impl = ctx.reflect._getImpl(name, strict = true)   // only ACTIVE providers
  if !impl or (impl.check && !impl.check()) → delete _store[name]
  else _store[name] = impl

_refresh():                             // recompute the target digest
  epoch = `${_revision}`
  for name of inject: if !_store[name] { epoch = INACTIVE; break } ; epoch += ':' + impl.uid
  _setEpoch(epoch)
```
The epoch string is the paper's hash of `target(γ, n)`, plus the revision counter (a configuration revision is a new effect function, §4.4 Configuration).

## 5. The transition state machine (`fiber.ts:431-514`)

### `_setEpoch(epoch)` – refresh (Algorithm 5, `refresh`)
```ts
if epoch === old → return                       // neutral change
if this._error → return                         // FAILED fibers only recover via update()/replace()
_runner.epoch = epoch                           // record the new target
if this.inertia → return                        // transition in flight: inertial
_updateState(() =>
  epoch !== INACTIVE && old === INACTIVE
    ? (inertia = _reload(), LOADING)            // L-Begin
    : (inertia = _unload(), UNLOADING))         // L-Leave (also for "target changed" while ACTIVE)
```
A change from one non-⊥ target to another while ACTIVE goes through `UNLOADING` and then chains into `LOADING` (unload checks the epoch at its end).

### `_reload()` (L-Begin … L-Finish / L-Divert)
```ts
committed = { ..._store }                               // commit the view ω
for impl of committed (not own): impl.dependents.add(this)   // register as dependent (for the guard)
oldEpoch = _runner.epoch
await Promise.resolve()                                 // let superseding updates coalesce
if epoch === oldEpoch: await _execute(_runner)          // run apply; async iterators check epoch per yield
catch (e): log; if epoch === oldEpoch { _error = e; epoch = INACTIVE }   // failure only if still current
_updateState(() => epoch === oldEpoch
   ? (inertia = undefined)                              // → ACTIVE (or FAILED via _getState)
   : (inertia = _unload(), UNLOADING))                  // stale ⇒ inertial chain into unload
```
*Inertia:* a stale target does not abort an in-flight `apply`; the load lands and the fiber unloads afterwards (async hosts take the *landing* alternative of L-Divert). The microtask yield before executing lets a burst of `update()` calls skip superseded loads entirely (`update.spec.ts: 'skips superseded loads before executing the plugin'`).

### `_unload()` (guard + L-Unload)
```ts
while ([...provisions].some(impl => impl.dependents.size))   // the guard (per binding)
  await new Promise(r => this._release = r)                   // woken by the last dependent
await Promise.all(_disposables.clear().map(async d => { await Promise.resolve(); await d() /* errors logged */ }))
for impl of committed:                                         // release our providers
  if impl.dependents.delete(this) && !impl.dependents.size:
    impl.release?.(); impl.ctx.fiber._release?.()
committed = undefined                                          // discard ω last
_updateState(() => epoch === INACTIVE
   ? (inertia = undefined)                                     // → PENDING / FAILED / DISPOSED
   : (inertia = _reload(), LOADING))                           // chain back into reload
```

### `_updateState(cb)` – state change + provider notification (`fiber.ts:372-385`)
Sets the new state, emits `internal/status(fiber, oldState)`, and **only on transitions into or out of `ACTIVE`** notifies dependents of every provision still bound in the store (`ctx.isolate(name, impl.isolate).reflect.notify([name])`). Entering `UNLOADING` from `ACTIVE` therefore makes all consumers recompute an unsatisfied target *before* any inverse is scheduled (L-Leave), while the bindings are still physically present.

### The two guards
1. **Fiber-level** (`_unload` loop): a provider fiber does not run *any* disposer until no provision has dependents.
2. **Binding-level** (`provide`'s inverse): withdrawing one binding waits for that binding's dependents only. This lets one provider withdraw binding A without waiting for consumers of binding B (`service-dispose.spec.ts: 'withdraws one binding without waiting for another binding from the same provider'`), and lets a binding be replaced during a consumer's initialization.

Dependents are tracked as `Impl.dependents: Set<Fiber>` (populated at commit, cleared at the end of the dependent's unload) – the runtime form of `relied_n`.

## 6. Waiting, updating, restarting (`fiber.ts:516-552`)

* `await()` – loop on `inertia` until settled; throw `_error` if FAILED.
* `restart()` – on `ctx.fiber`: `_revision++`, `_refresh()`, await. For the root fiber `dispose()` *is* `restart()`: everything is unloaded and the root becomes active again.
* `update(config, noSave = false)` – the configuration revision path:
  1. resolve/validate the new config if loaded (`internal/config` waterfall → Standard Schema `validate`, sync only; `ValidationError` keeps issue paths);
  2. `emit('internal/commit', fiber, config)` unless `noSave` (the loader writes the config back to its entry);
  3. store config, clear `_error`;
  4. if loaded, `waterfall(fiber, 'internal/update', resolved, restart)` – per-fiber `internal/update` listeners (registered by the plugin via `ctx.on('internal/update', …)`) may accept the new config **without restarting** by not calling `next()`; otherwise `restart()`.
  The returned promise is marked handled so dropped results do not cause unhandled rejections.

Concurrency semantics verified by `tests/update.spec.ts`: concurrent updates settle on the latest config; updates during unloading coalesce; a superseded failure is cleaned up without failing the latest update; all waiting updates reject when the latest load fails; an outdated async generator stops advancing; a queued config is not loaded after disposal.

## 7. Failure semantics (`tests/fiber.spec.ts`)
* A throwing `apply` → error logged, `_error` set, epoch ⊥, partial effects unwound via `_unload`, state `FAILED`.
* A failed fiber **does not re-enter** on dependency refresh (`_setEpoch` returns early) – matches §4.4 "not retried against an unchanged environment".
* `update()` (or `registry.replace`) clears the error and retries.
* Disposing a PENDING or FAILED fiber settles it to `DISPOSED`.

## 8. Mapping to the nine rules

| Rule | Code |
|---|---|
| O-Insert | `parent.fiber.effect(() => { runtime.fibers.push(this); this._refresh() … })` |
| O-Retire | inverse of that effect: `uid = null; _setEpoch(INACTIVE)` |
| O-Remove | `_remove()`, runtime unregistered, state `DISPOSED` after the chain ends |
| L-Begin | `_setEpoch` → `_reload()` with state `LOADING` |
| L-Iter | async iterator loop in `_execute` while `epoch === oldEpoch` |
| L-Finish | `_reload` end with unchanged epoch → `ACTIVE` |
| L-Divert | epoch changed during load → iterator stops at next boundary, `_reload` chains into `_unload` |
| L-Leave | `_setEpoch` → `_unload()` with state `UNLOADING`; `_updateState` notifies consumers |
| L-Unload | `_unload` after the dependents guard |
