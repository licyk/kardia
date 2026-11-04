# Paper vs. Implementation: Correspondence and Deviations

> **Layers compared:** Theory (the paper's model and its pseudo-code, §3–5) and Implementation (the TypeScript code at `cordis/`). This document is the bridge used by the Rust plan: where the two differ, the plan states which one it follows and why.

## 1. Faithful correspondences

| Concept | Paper | Code | Notes |
|---|---|---|---|
| Single mutation primitive | `ctx.effect` (§5.1.1) | `Fiber.effect` (`fiber.ts:292`) | provide, plugin, on, accessor, mixin, exporter, watcher… all reduce to it |
| Effect function / iterator | 𝔈Γ / ℑΓ | function / (async) generator returning/yielding disposers | `_execute` (`fiber.ts:240`) |
| LIFO within an effect | Def. 18 | `disposables.splice(0).reverse()` sequential | `fiber.ts:298` |
| At-most-once disposal | `armed` | `runner.epoch = false` | `fiber.ts:339` |
| Iteration-boundary guard | `execute(cb, guard)` | `while (runner.epoch === oldEpoch)` for async iterators | sync iterators unguarded |
| Single source per key | O-Insert premise | `provide` throws if `store[realm]` occupied | per realm (§4.4 Isolation) |
| Provider available only while Active | eq. 46 | `_getImpl(name, strict)` checks `state === ACTIVE` | |
| Provider identity, not value | Def. 53 | `Impl.uid` (fresh counter) in the epoch string | |
| Committed view ω | `fiber.committed` | `committed = { ..._store }` at reload start, cleared last in unload | |
| L-Leave before any inverse | Algorithm 5 line 10 | `_updateState` notifies consumers on leaving ACTIVE before `_unload` disposes | |
| Guard on L-Unload | Algorithm 5 line 25 | `_unload` waits while any provision has dependents; `provide` inverse waits per binding | see §2.3 |
| Inertia | §4.4 | `inertia` promise; `_setEpoch` returns early while a transition is in flight; `_reload`/`_unload` chain | |
| Failure outcome | §4.4 Failure | `_error`, state `FAILED`, no re-entry until `update`/`replace` | |
| Instantiation as parent effect | Def. 52, Algorithm 4 | `parent.fiber.effect(...)` in the `Fiber` constructor | |
| Derived realization | Def. 23 | `ctx.extend` prototype children for isolate/intercept/fiber contexts | |
| Two-layer isolation | Def. 24 | `ctx[isolate][name]` symbol → `reflect.store[symbol]` | |
| Interception merge | Def. 27 | `resolveIntercept`: declared `inject[name]` ⊕ outer→inner context intercepts, right-biased | |
| Committed-view access | Algorithm 6 | Proxy `get` → `locate` | plus own provisions and isolation boundaries |
| Entry fields | Def. 81 | `EntryOptions` | adds `inject`, `group` |
| Managed realms + delimiters | Algorithm 7 | `loader/src/config/isolate.ts` | plus migration of the provider's own impl without restart |

## 2. Deviations and refinements

### 2.1 Target representation
* **Paper:** `fiber.target` is a hash of `target(γ, n)` (key → provider uid tuple), ⊥ when unsatisfied.
* **Code:** `_runner.epoch` = `"<revision>:<uid>:<uid>…"` or `'__INACTIVE__'`. The **revision** counter folds configuration revisions into the target: `restart()`, `update()` and `registry.replace()` bump it, forcing an unload→reload chain without retiring the fiber. This realizes the §4.4 *Configuration* revision more cheaply than retire → remove → reinsert at the same name.

### 2.2 Retirement and removal
* **Paper:** O-Retire (set τ) and O-Remove (drop entry) are separate rules; a retired entry may linger (vestigial).
* **Code:** the instantiation effect's inverse sets `uid = null` (removed from the runtime immediately), forces the target to ⊥, then awaits the transition chain; the final state is `DISPOSED`. There is no persistent registry of retired entries. Re-enabling an entry creates a **fresh fiber** (as the paper prescribes).

### 2.3 Guard implementation
* **Paper (Algorithm 5):** `unload` awaits `notify(...)`'d dependents' `await()`.
* **Code:** explicit reverse references `Impl.dependents: Set<Fiber>` populated at commit time, removed at the end of the dependent's unload, plus wake-up resolvers (`impl.release`, `fiber._release`). Two guards exist: the **fiber-level** guard (no disposer of the provider runs until *all* its provisions are free) and the **binding-level** guard inside `provide`'s inverse. This gives the paper's per-binding semantics (`relied_n`) while letting a provider withdraw one binding without waiting for another.

### 2.4 Accumulator execution order
* **Paper:** accumulator = LIFO composite; Algorithm 5 runs `fiber.dispose()` once.
* **Code:** fiber-level disposers are *initiated* in reverse order but run **concurrently** (`Promise.all`), each after one microtask; errors are logged and do not stop others. Sequential LIFO holds only *inside* one `ctx.effect`. The paper acknowledges the concurrency (§5.1.3) and it is justified by Theorem 43 (independent inverses commute), but it means ordering-sensitive teardown must be grouped into one effect.

### 2.5 Iteration boundaries
* **Paper:** the guard is checked before each iteration; L-Divert may abort or land.
* **Code:** only **async** iterators are interruptible; sync iterators and plain functions run to completion (they cannot be interrupted in a single-threaded host anyway). Async iterators take the *landing* alternative (inertial host) and are closed with `iter.return()` so `finally` blocks run. `_reload` also yields one microtask before executing, so superseded loads never start.

### 2.6 Notification
* **Paper:** `notify` iterates all fibers and calls `refresh`.
* **Code:** iterates all runtimes' fibers, filters by declared key and **realm symbol** (or a custom filter during realm moves), recomputes only the affected keys (`_checkImpl`), then `_refresh`. It also emits `internal/service`. Notifications for a fiber's provisions are issued from `_updateState` only on transitions into/out of `ACTIVE`.

### 2.7 Availability predicate
The code adds `Impl.check` (from `Service[symbols.check]`): a provider may report itself unavailable even while ACTIVE (used by `loader` for `{ await: true }`). This extends the satisfaction predicate `σ ⊧ d` with a per-provider condition.

### 2.8 Access resolution
`locate` additionally (a) lets a fiber read its **own** provisions without declaring them and (b) stops inheritance at an **isolation boundary**. The root fiber (no runtime) gets *unchecked* access (`reflect.get(prop, false)`), because root code cannot declare `inject`.

### 2.9 Caller attribution (not in the paper)
The tracker/shadow proxy mechanism ([05](05-services-and-traceability.md)) attributes effects created inside service methods to the **calling** fiber and resolves the service's own dependencies from its **def site**. The paper's confinement (Def. 55) implicitly assumes this; without it, service-mediated effects would belong to the provider and break temporal composability for consumers.

### 2.10 Loader specifics not in the paper
* Two fibers per entry with `inject` (outer dependency holder + inner plugin).
* Expressions (`!!js`) evaluated in specific contexts; cached `disabled` verdict.
* Self-dispose detection → persisted `disabled: true`; self-update → persisted config (`internal/commit`).
* `loader { await: true }` barrier.
* Realm migration without restarting the provider; blocked moves tracked via `Impl.isolate`.

### 2.11 HMR
See [09-hmr §6](09-hmr.md#6-paper-vs-code): registry-level in-place replacement (`registry.replace` + `_replace` + revision bump) instead of dispose + `ctx.use`; union-find commit units instead of all-or-nothing; no rollback after the point of no return.

### 2.12 Things the paper assumes that the code does not check
* Effect witnesses (inverse really reverts) – unchecked, author obligation (stated in the paper).
* Coeffect commutativity – unchecked, provider obligation (stated in the paper).
* Acyclicity of ≺ – **not detected**: a dependency cycle leaves fibers PENDING silently; the paper (§6.5) says a runtime *can* report it at load time, but Cordis does not.
* Totality on provision (Def. 76) – not enforced; `provide` metadata on plugins is declarative only.
* Confinement (Def. 55) – enforced only for coeffect *access* through the proxy; arbitrary JS can still reach globals.

## 3. Defects observed while reading
| Location | Observation |
|---|---|
| `loader/src/config/isolate.ts:150-153` | `return` inside the realm-GC scan exits the whole listener; GC for the remaining names of that entry's `legacy.isolate` is skipped. `continue` of the outer loop was likely intended. |
| `create/src/index.ts` | `--mirror` flag parsed but unused. |

## 4. Implications for the Rust plan
1. Follow the **code's** state machine (epoch with revision, inertia, two-level guard, `FAILED`, `DISPOSED`), because it is tested and handles configuration revisions; keep the paper's names in docs.
2. Follow the **paper** where the code is constrained by JS: make iteration boundaries explicit for all multi-step effects; detect dependency cycles at insertion time (cheap, declarations are static).
3. Replace Proxy-based caller attribution with explicit typed handles.
4. Follow the **code's** HMR design (unit-wise commit, in-place replacement) over Algorithm 10.
5. Fix the realm-GC early `return` in the port.
