# Rust Implementation Plan for Cordis (`cordis-rs`)

> **Layer:** Proposal. This document designs a Rust re-implementation of the framework. It builds on
> * the **theory** – [`paper/`](paper/01-overview.md) (definitions, rules, theorems), and
> * the **reference implementation** – [`implementation/`](implementation/01-repository-overview.md) (TypeScript, Cordis v4),
>
> and is explicit about which of the two it follows where they differ ([`implementation/11-paper-vs-implementation.md`](implementation/11-paper-vs-implementation.md)). The plan is implemented in [`../crates/`](../crates/); §15 records the status and where the implementation departs from this text.

---

## 1. Goals, non-goals, guiding decisions

### Goals
1. **Semantic fidelity to the calculus** (paper §4): fibers, target vs. committed views, the guard on unload, inertia, failure outcomes, single source per (key, realm), confluence.
2. **Behavioural parity with Cordis core** where the paper is silent (epoch with revision, two-level guard, concurrent fiber-level disposal, `update` veto, `replace`), using the TypeScript test suite as the acceptance specification.
3. **Idiomatic Rust**: typed service keys, explicit caller attribution instead of `Proxy`, `Send + Sync` handles usable from a multi-threaded tokio runtime, no `unsafe` in the core.
4. A declarative **loader** (entries, groups, realms) and file-backed **include** with two-way sync.

### Non-goals (for the first releases)
* Re-evaluating plugin *code* at runtime (HMR). Rust has no module registry; this becomes an optional, later "module source" extension (§10).
* npm-specific tooling (`package-manager`, `create`, `loader-utils`). Not ported.
* Byte-exact parity of error messages and UTF-16 column numbers (nice-to-have, not required).

### Guiding decisions (summary; justified in the sections below)
| # | Decision | Section |
|---|---|---|
| D1 | `Send + Sync` core; one state lock per root; **no user code runs while the lock is held**; every JS "synchronous segment" becomes one critical section; follow-up work is returned as an action list and executed after unlocking | §4 |
| D2 | Fibers live in a registry arena addressed by never-reused `FiberId`s; all cross references (parent, dependents, committed view) are ids, never pointers | §5 |
| D3 | Service keys are **strings** (needed by configuration) bound to a **Rust type** via a `Service` trait; values stored as `Arc<dyn Any + Send + Sync>` plus their `TypeId` (detects key collisions) | §6 |
| D4 | Caller attribution by an explicit `Svc<S>` handle = (service `Arc`, caller `Context`); services keep their own def-site `Context` | §6.4 |
| D5 | Effects are registered on the current fiber's accumulator; disposers are `FnOnce` (at-most-once enforced by the type system); `EffectHandle` does **not** dispose on `Drop` | §7 |
| D6 | Async transitions are **inertial**: an in-flight `apply` future is never dropped; divert happens at explicit checkpoints (`ctx.checkpoint().await?`, `scope.yield_(d).await?`) | §7.3 |
| D7 | Configuration enters as `serde_json::Value` and is deserialized **once** into `P::Config` at the boundary | §8 |
| D8 | Plugins are statically registered in a `Catalog` (name → erased factory); dynamic modules are a later, pluggable backend | §9, §10 |
| D9 | Dependency cycles are reported when a fiber is inserted (paper §6.5), not silently left pending | §5.6 |

---

## 2. Concept mapping: paper → Cordis (TS) → Rust

| Paper | Cordis (TypeScript) | Rust (proposed) |
|---|---|---|
| Γ∞ context | `Context` (Proxy, prototype chain) | `Context` – cheap `Clone` handle: `Arc<Root>` + `FiberId` + `Arc<Scope>` |
| derived realization | `ctx.extend()`, `isolate`, `intercept` | `ctx.isolate(..)` / `ctx.intercept(..)` return a new `Context` with a child `Scope` |
| effect / effect iterator | `ctx.effect(fn | generator)` | `ctx.effect(label, \|scope\| async {..})`, `scope.defer(..)`, `scope.yield_(..).await?` |
| accumulator g | `fiber._disposables: DisposableList` | `FiberData::disposables: IdTable<EffectId>` |
| inverse | `() => …` | `Disposer = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>` |
| Σ, Σ_iso | `reflect.store: Dict<Impl, symbol>`, `ctx[isolate]` | `State::store: HashMap<RealmId, Impl>`, `Scope::isolate: overlay HashMap<Key, RealmId>` |
| Σ_inter | `ctx[intercept]` | `Scope::intercept: overlay HashMap<Key, serde_json::Value>` |
| key k / value 𝒱_k | string name / `any` | `Key(Arc<str>)` / `S: Service`, `TypeId` checked |
| provider identity | `Impl.uid` | `ImplId(u64)` (never reused) |
| component (d, p, e) | plugin + `inject` + `provide` | `trait Plugin { const NAME; type Config; fn inject(); fn provide(); fn apply(); }` |
| fiber | `Fiber` | `FiberData` in the registry + `Fiber` handle |
| name 𝔑 | `fiber.uid` | `FiberId(u64)` |
| θ | `FiberState` | `enum FiberState { Pending, Loading, Active, Unloading, Failed, Disposed }` |
| target view | `_runner.epoch` string | `Target::{Inactive, Epoch { revision, providers }}` (structured, not a string) |
| committed view ω | `fiber.committed` | `Arc<CommittedView>` (key → `ImplId`) |
| inertia | `fiber.inertia: Promise` | `Option<Shared<BoxFuture<'static, ()>>>` + spawned task |
| guard (relied_n) | `Impl.dependents`, `_release`, `impl.release` | `Impl::dependents: HashSet<FiberId>` + `tokio::sync::Notify` |
| O-Insert / O-Retire | `ctx.plugin()` / its effect's inverse | `ctx.plugin::<P>(cfg)` / the returned instantiation disposer |
| failure outcome | `_error`, `FAILED` | `FiberData::error: Option<Arc<PluginError>>`, `FiberState::Failed` |
| proxy access check (Alg. 6) | `ReflectService.handler` + `locate` | `ctx.get::<S>() -> Result<Svc<S>, AccessError>` walking committed views |
| caller attribution | tracker / shadow proxies | `Svc<S>::caller()`; services hold their own `Context` |
| events | `EventsService` string events | typed events: `trait Event` + `ctx.on::<E>()`, dispatch modes |
| loader entry | `EntryOptions`, `Entry` | `EntryOptions` (serde), `Entry` state machine in `cordis-loader` |
| module import | Node ESM loader | `Catalog` of statically registered plugins; optional `ModuleSource` backends |

---

## 3. Workspace layout

A Cargo workspace at the repository root (next to the untouched `cordis/` reference code and `docs/`):

```
cordis-rs/
├─ Cargo.toml                 # [workspace]
├─ crates/
│  ├─ cordis-core/            # Phases 1–3: the paper's core library
│  │  └─ src/
│  │     ├─ lib.rs            # public re-exports
│  │     ├─ root.rs           # Root (shared state + lock), Context handle
│  │     ├─ scope.rs          # Scope overlays: isolate / intercept tables, re-parenting
│  │     ├─ key.rs            # Key, RealmId, ImplId, FiberId, EffectId newtypes
│  │     ├─ effect.rs         # effect engine, EffectScope, Disposer, EffectHandle, IdTable
│  │     ├─ store.rs          # Impl, provide / withdraw / get, notify
│  │     ├─ fiber.rs          # FiberData, FiberState, Target, CommittedView, Fiber handle
│  │     ├─ lifecycle.rs      # refresh / reload / unload state machine (pure transitions + actions)
│  │     ├─ registry.rs       # runtimes, plugin(), replace(), aliases
│  │     ├─ plugin.rs         # Plugin trait, closures as plugins, erased PluginDef
│  │     ├─ service.rs        # Service trait, Svc<S>, interception metadata merge
│  │     ├─ access.rs         # committed-view resolution (Algorithm 6 + isolation boundaries)
│  │     ├─ events.rs         # typed events, dispatch modes, internal hook points
│  │     ├─ logger.rs         # Logger service, exporters-as-effects, ring buffer
│  │     ├─ config.rs         # Value → Config deserialization, ValidationError with paths
│  │     └─ error.rs          # CordisError, AccessError, PluginError
│  ├─ cordis-macros/          # Phase 6: optional proc macros (ergonomics, compile-time inject checks)
│  ├─ cordis-timer/           # Phase 3: timer service (tokio::time)
│  ├─ cordis-logger-console/  # Phase 3: console exporter
│  ├─ cordis-fs/              # Phase 5: fs.watch service (notify + globset)
│  ├─ cordis-loader/          # Phase 4: entries, tree, group, realms, catalog, validation, expressions
│  ├─ cordis-include/         # Phase 5: YAML/JSON-backed trees: parse, journal, patch, sync, atomic write
│  ├─ cordis-hmr/             # Phase 7 (optional, research): module graph + reconcile algorithm
│  ├─ cordis-guest/           # guest SDK: write WASM / dylib guest plugins in Rust
│  └─ cordis/                 # facade crate re-exporting the above + `cordis` binary (bootstrap like core/bin.js)
└─ docs/
```

Crate dependency direction mirrors the TS packages: `core ← timer, logger-console, fs ← loader ← include ← cordis (facade)`; `hmr` depends on `core` (+ `loader` only for tests).

### 3.1 External dependencies
Kept deliberately small; each line says why it is needed.

| Crate | Used by | Purpose |
|---|---|---|
| `tokio` (`rt`, `sync`, `time`, `macros`) | core, timer, include | task spawning for transitions, `Notify`/`watch`, timers; `test-util` for paused time in tests |
| `futures` | core | `BoxFuture`, `Shared` (inertia handles), `join_all`, `catch_unwind` |
| `parking_lot` | core, loader | the per-root state lock (non-poisoning, small) |
| `smallvec` | core | small inline vectors for targets / committed views |
| `thiserror` | all | structured error enums |
| `serde`, `serde_json` | core, loader, include | configuration values, interception metadata |
| `serde_path_to_error` | core | validation issues with paths (Standard Schema analogue) |
| `indexmap` | loader, include | ordered entry options |
| YAML parser with spans and tags (`saphyr` family) | include | `!!js` tags, source locations |
| `tempfile` | include | atomic temp-file writes |
| `notify`, `globset` | fs | file watching, glob filters |
| `anstream` / `anstyle` | logger-console | coloured output |
| `proptest` (dev) | core | model-based tests of the metatheory |
| `trybuild` (dev, Phase 6) | macros | compile-fail tests for undeclared access |
| `wasmtime` / `libloading` (Phase 7, optional) | hmr backends | dynamic modules |

Not used: `async-trait` (return-position `impl Future` in traits is stable), `anyhow` in library crates (only in examples/binaries), `inventory`/`linkme` (explicit `Catalog::register` is enough).

---

## 4. Concurrency and execution model (D1)

### 4.1 Why the model matters
Cordis's correctness rests on JavaScript's run-to-completion semantics: code between two `await`s is atomic. Many algorithms (inertia in `_setEpoch`, `_initTask ??=` in the loader, "assign options synchronously then commit") depend on this. A Rust port must reproduce *that atomicity*, not JS's single-threadedness per se.

### 4.2 Chosen model
* All mutable runtime state of one root lives in `Root { state: parking_lot::Mutex<State>, … }`.
* **Porting rule:** every synchronous segment of the TS code becomes **one critical section** on `State`. Nothing awaits while holding the lock, and **no user code is called while holding the lock** (plugin `apply`, disposers, event listeners, availability checks).
* Critical sections are written as functions `fn step(&mut State, …) -> Actions`, where `Actions` is a small list of deferred work: `Spawn(Transition)`, `Emit(InternalEvent)`, `Wake(Notify)`, `RunDisposers(Vec<Disposer>)`. A tiny driver executes the actions after releasing the lock. This keeps the state machine **pure and unit-testable** and removes the reentrancy hazards JS hides (in TS, `_updateState` synchronously emits `internal/status` to user listeners from inside the state machine).
* Transitions (`reload`, `unload`) are spawned tokio tasks. The paper's footnote 2 anticipates this: *"with lazy scheduling (e.g. … Rust futures) the host must spawn the task for it to progress."*
* Handles (`Context`, `Fiber`, `Svc<S>`) are `Send + Sync`; plugin futures must be `Send`. Users can freely `tokio::spawn` inside plugins (and tie the `JoinHandle` to an effect).
* Tests run on a `current_thread` runtime with `tokio::time::pause()` for deterministic interleavings (the analogue of vitest fake timers); production may use the multi-threaded runtime.

**Alternative considered:** a `!Send` design (`Rc<RefCell<..>>` on a `LocalSet`) mirrors JS most literally and was recommended by one analysis pass for the loader. It is rejected for the core because it would force every plugin and service in the ecosystem to be `!Send`-compatible (no `tokio::spawn`, awkward with most async libraries), while the lock-plus-actions discipline above preserves the same atomicity with comparable complexity. If profiling later shows lock contention on the access hot path, committed views (immutable per episode) can be published through `arc-swap` without changing semantics.

### 4.3 Lock ordering
`core::State` is the innermost lock. Higher layers (`loader` tree state, `include` IO state) have their own mutexes and may call into core only **after releasing** their own lock or while holding it **only if core never calls back into them under its lock** (which D1 guarantees). Rule of thumb: *take at most one lock at a time*.

---

## 5. Core data structures

### 5.1 Identifiers (`key.rs`)
```rust
pub struct Key(Arc<str>);                 // service name, e.g. "database", "foo.bar"
pub struct RealmId(u64);                   // realm symbol; debug label "db#entry1" / "db@shared"
pub struct ImplId(u64);                    // provider binding identity, never reused
pub struct FiberId(u64);                   // fiber name (paper 𝔑), never reused; 0 = root
pub struct EffectId(u64);
```
Never-reused ids implement the paper's atomic generative names and make "replaced by an equal value" observable (Def. 53).

### 5.2 Scope overlays (`scope.rs`) – derived realization and live re-parenting
```rust
pub struct Scope {
    parent: RwLock<Option<Arc<Scope>>>,               // re-pointable (loader moves entries)
    isolate: RwLock<HashMap<Key, RealmId>>,           // own overrides (ρ)
    intercept: RwLock<HashMap<Key, serde_json::Value>>,// own metadata (ι)
    entry_marker: Option<EntryMarkerId>,              // loader: "contexts inside entry E"
}
impl Scope {
    fn realm(&self, key: &Key, root_realms: &RootRealms) -> RealmId;  // nearest override, else root realm of key
    fn has_own_isolate(&self, key: &Key) -> bool;                     // isolation boundary test (locate)
    fn intercept_chain(&self, key: &Key) -> Vec<serde_json::Value>;   // outer → inner
}
```
This replaces JS prototype chains (`Object.create`, `setPrototypeOf`, `swap`). Own vs. inherited is explicit.

### 5.3 Bindings (`store.rs`)
```rust
pub(crate) struct Impl {
    id: ImplId,
    key: Key,
    realm: RealmId,                        // where it actually lives (may differ after a blocked realm move)
    provider: FiberId,
    value: Arc<dyn Any + Send + Sync>,
    type_id: TypeId,                       // detects key collisions across unrelated types (paper §6.6)
    check: Option<Arc<dyn Fn() -> bool + Send + Sync>>,  // extra availability predicate; must not touch the runtime
    dependents: HashSet<FiberId>,          // fibers whose committed view names this impl (relied_n)
    released: Arc<Notify>,                 // woken when `dependents` becomes empty
}
pub(crate) struct State {
    fibers: HashMap<FiberId, FiberData>,
    runtimes: HashMap<RuntimeKey, RuntimeData>,
    aliases: HashMap<RuntimeKey, RuntimeKey>,   // registry.replace forwarding
    store: HashMap<RealmId, Impl>,
    root_realms: HashMap<Key, RealmId>,         // lazily assigned root realm per key
    props: HashMap<Key, PropKind>,              // service vs accessor declarations
    counters: Counters,
}
```

### 5.4 Fibers (`fiber.rs`)
```rust
pub enum FiberState { Pending, Loading, Active, Unloading, Failed, Disposed }

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Inactive,                                   // ⊥
    Epoch { revision: u64, providers: SmallVec<[ImplId; 4]> },   // ordered by inject declaration
}

pub(crate) struct FiberData {
    id: FiberId,
    parent: FiberId,                            // root's parent is itself
    scope: Arc<Scope>,                          // the fiber's derived context scope
    runtime: RuntimeKey,
    inject: Arc<InjectSpec>,                    // d (+ declared interception metadata)
    config: Arc<ConfigSlot>,                    // raw Value + typed config (validated once)
    state: FiberState,
    revision: u64,                              // bumped by restart / update / replace
    resolved: HashMap<Key, ImplId>,             // live resolution per declared key (_store)
    target: Target,                             // what it should run against (_runner.epoch)
    committed: Option<Arc<CommittedView>>,      // ω, fixed for an episode
    provisions: SmallVec<[ImplId; 2]>,          // σ_n
    disposables: IdTable<EffectEntry>,          // accumulator, insertion-ordered
    inertia: Option<Shared<BoxFuture<'static, ()>>>,
    error: Option<Arc<PluginError>>,            // failure outcome
    removed: bool,                              // uid === null in TS
    unload_gate: Arc<Notify>,                   // fiber-level guard wake-up (_release)
    status_tx: watch::Sender<FiberState>,       // for Fiber::settled().await
}
pub struct CommittedView { entries: SmallVec<[(Key, ImplId); 4]> }
```

### 5.5 `IdTable<T>` – commutative-by-construction registry
The `DisposableList` design (insert returns a fresh id; remove by id; `drain_rev()` returns LIFO order) is exported publicly as `cordis_core::IdTable<T>`. It is what paper §3.4.2 calls a commutative key: service authors should build their registries (routes, listeners, commands) on it.

### 5.6 Dependency-cycle detection (D9)
On insertion, compute whether the precedence relation ≺ (`p_n ∩ d_m ≠ ∅`, from declared `provide`/`inject`) closes a cycle among non-disposed fibers in the same realm; if so, log a warning naming the cycle and leave the fiber `Pending` (behaviour otherwise unchanged). It only reads declarations, so it can be computed eagerly (paper §6.5). Declared `provide` is metadata only (as in Cordis); a plugin that provides undeclared keys simply escapes the check.

---

## 6. Services, keys and access

### 6.1 The `Service` trait
```rust
pub trait Service: Send + Sync + 'static {
    /// Coeffect key. Strings (not TypeIds) because configuration names services.
    const NAME: &'static str;
    /// Interception metadata (paper Def. 26); merged right-biased.
    type Meta: Mergeable + DeserializeOwned + Default + Send + Sync + 'static = ();  // (associated type defaults are unstable → use a separate `Intercept` trait in practice)
}
pub trait Mergeable { fn merge(self, inner: Self) -> Self; }   // default for JSON objects: shallow assign
```
(Associated type defaults are not stable; the implementation will put `Meta` on a separate `trait Intercepted: Service` with a blanket "no metadata" default.)

### 6.2 Providing
```rust
impl Context {
    pub fn provide<S: Service>(&self, value: S) -> Result<EffectHandle, CordisError>;
    pub fn provide_arc<S: Service>(&self, value: Arc<S>) -> Result<EffectHandle, CordisError>;
}
```
Semantics follow `ReflectService.provide` exactly: a tracked effect; single source per realm (`AlreadyProvided { key, by }`); notify immediately only if the providing fiber is `Active`, otherwise on its transition to `Active`; the inverse removes the binding, notifies the realm, **waits until `dependents` is empty**, then removes the provision. In-place overwrite (`reflect.set`) is offered as `ctx.replace_value::<S>(..)` and, as in Cordis and the paper, is *not* observed by dependents.

### 6.3 Accessing (Algorithm 6)
```rust
impl Context {
    pub fn get<S: Service>(&self) -> Result<Svc<S>, AccessError>;      // checked: committed view walk
    pub fn try_get_live<S: Service>(&self) -> Option<Svc<S>>;          // reflective ctx.get: live store, never fails
    pub fn get_dyn(&self, key: &str) -> Result<DynSvc, AccessError>;   // by name (loader expressions, tooling)
}
pub enum AccessError {
    Undeclared { key: Key },                   // "cannot get property without inject"
    Inactive { key: Key },                     // "cannot get required service in inactive context"
    TypeMismatch { key: Key, expected: &'static str },
}
```
`get` implements `locate`: own provisions in the same realm → walk fibers upward → stop at an isolation boundary → the first fiber whose `committed` has the key returns it; a fiber that declares the key without a committed impl → `Inactive`; reaching the root → root has **unchecked** access to the live store (as in Cordis). Reading the committed view (not the store) keeps a dependency readable during the consumer's own teardown (Theorem 70).

Typed declarations: `Plugin::Inject` can be a tuple of service types (`(Database, Cache)`), implemented with `macro_rules!` over tuples, producing both the runtime `InjectSpec` and a typed accessor `ctx.deps::<P::Inject>() -> Result<(Svc<Database>, Svc<Cache>)>`. This is the first, macro-free step towards the compile-time mediation the paper suggests (§5.1.4, §6.4).

### 6.4 Caller attribution (D4)
```rust
pub struct Svc<S: ?Sized> { inner: Arc<S>, caller: Context, impl_id: ImplId }
impl<S: ?Sized> Deref for Svc<S> { type Target = S; }
impl<S: ?Sized> Svc<S> { pub fn caller(&self) -> &Context; pub fn intercept(&self) -> S::Meta where S: Intercepted; }
```
* Effects created *on behalf of the caller* use `svc.caller().effect(..)` – they land in the caller's fiber, exactly what Cordis's tracker achieves with `this.ctx`.
* A service resolves *its own* dependencies through the `Context` it stored at construction (its def site), so no shadow contexts are needed.
* Service crates expose caller-aware APIs as **extension traits on `Svc<S>`** (allowed by coherence because the trait is local):
```rust
pub trait TimerExt { fn timeout(&self, d: Duration, f: impl FnOnce() + Send + 'static) -> EffectHandle; }
impl TimerExt for Svc<Timer> { fn timeout(&self, d, f) -> EffectHandle { self.caller().effect_sync("timer.timeout", …) } }
```
* Interception metadata is resolved at access time per Def. 27 / `resolveIntercept`: `[declared inject meta, outer…, inner]` merged right-biased.

### 6.5 Associated properties / mixins
Cordis's `foo.bar` associated services and `ctx.mixin` are JS-object conveniences. In Rust: a service named `foo.bar` is just another key; "extending another plugin's type" is done with extension traits (compile time) or with a registry service built on `IdTable` (runtime). No accessor/mixin layer is ported.

---

## 7. Effects (D5, D6)

### 7.1 API
```rust
pub type Disposer = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

impl Context {
    /// Effect function: run `f`, which returns its inverse.
    pub fn effect_sync<F>(&self, label: &'static str, f: F) -> Result<EffectHandle, CordisError>
        where F: FnOnce() -> Option<Disposer>;
    /// Effect iterator: `f` receives a scope; each `yield_` is an iteration boundary.
    pub fn effect<F, Fut>(&self, label: &'static str, f: F) -> Result<EffectHandle, CordisError>
        where F: FnOnce(EffectScope) -> Fut, Fut: Future<Output = Result<(), PluginError>> + Send + 'static;
    /// Register a plain inverse on the current fiber (e.g. inside `apply`).
    pub fn on_dispose(&self, d: impl FnOnce() + Send + 'static) -> EffectHandle;
    /// Iteration boundary for the fiber's own `apply` (L-Divert checkpoint).
    pub async fn checkpoint(&self) -> Result<(), Interrupted>;
}
pub struct EffectScope { /* local disposers, guard */ }
impl EffectScope {
    pub fn defer(&mut self, d: Disposer);                         // collect without a boundary
    pub fn adopt(&mut self, h: EffectHandle);                     // ownership transfer of a nested effect
    pub async fn yield_(&mut self, d: Disposer) -> Result<(), Interrupted>;  // collect + boundary check
}
#[derive(Clone)] pub struct EffectHandle { /* root, fiber, effect id */ }
impl EffectHandle {
    pub async fn dispose(&self);          // at-most-once; waits for setup to finish first
    pub async fn ready(&self) -> Result<(), PluginError>;  // `await ctx.effect(...)` in TS
}
```

### 7.2 Semantics (from `fiber.ts:292-357`)
* `assert_active`: effects on a removed fiber fail with `CordisError::InactiveEffect`.
* Inside one effect: disposers run **sequentially in reverse order**.
* Ownership transfer: a nested `EffectHandle` created inside an effect is registered on the fiber; `scope.adopt(h)` moves it under the outer effect (the TS `collect` → `_disposables.delete`).
* A failing effect body disposes what it collected; synchronous errors propagate to the caller, async ones are logged and reported via `ready()`.
* Fiber unload drains the accumulator in reverse insertion order and runs the disposers **concurrently** (`join_all`), logging failures and panics (`catch_unwind`), because Cordis tests require sibling effects to clean up concurrently (`service-dispose.spec.ts`). Ordered teardown is obtained by grouping steps in one effect.
* `EffectHandle` is a non-owning handle: dropping it does **not** dispose. Component lifetime is dynamic and governed by the fiber, which is exactly where the paper distinguishes Cordis from RAII (§7.3: lexical scoping is complementary, for resources local to one effect – inside a disposer, ordinary `Drop` types are fine).

### 7.3 Interruption and cancellation safety (D6)
Rust *can* cancel a future by dropping it – something JS cannot – so a Rust host could offer the paper's *aborting* L-Divert. The plan deliberately does **not** drop in-flight `apply` futures: a future dropped between "acquire resource" and "register its disposer" would leak exactly what temporal composability promises to recover. Instead:
* the host is **inertial** (paper §4.4): a stale load lands, then unloads;
* explicit boundaries – `scope.yield_(..).await?` and `ctx.checkpoint().await?` – return `Err(Interrupted)` when the fiber's target changed since the load began (the TS `while (runner.epoch === oldEpoch)` check). Returning `Interrupted` from `apply` is not a failure; the fiber unloads what it registered.
This is the loader's outer-fiber pattern (`yield () => {}` after import) made explicit.

---

## 8. Plugins, configuration and the lifecycle

### 8.1 Plugin trait
```rust
pub trait Plugin: Send + Sync + 'static {
    const NAME: &'static str;
    type Config: DeserializeOwned + Send + Sync + 'static;     // `()` for no config
    type Inject: InjectSet = ();                               // (via helper trait; defaults unstable)
    fn inject_meta() -> InjectSpec { Self::Inject::spec() }    // declared keys + interception metadata
    fn provide() -> &'static [&'static str] { &[] }            // declarative provision (p), for tooling & cycle check
    fn apply(ctx: Context, config: Arc<Self::Config>)
        -> impl Future<Output = Result<(), PluginError>> + Send;
}
impl Context {
    pub fn plugin<P: Plugin>(&self, config: P::Config) -> Result<Fiber, CordisError>;
    pub fn plugin_value<P: Plugin>(&self, config: serde_json::Value) -> Result<Fiber, CordisError>;
    pub fn inject<F, Fut>(&self, deps: InjectSpec, f: F) -> Result<Fiber, CordisError>;   // closure plugin
}
```
Erased form stored in runtimes: `PluginDef { key: RuntimeKey, name, inject: Arc<InjectSpec>, provide, decode: fn(&Value) -> Result<ErasedConfig, ValidationError>, apply: Arc<dyn Fn(Context, ErasedConfig) -> BoxFuture<'static, Result<(), PluginError>> + Send + Sync> }`. `RuntimeKey` is `TypeId::of::<P>()` for typed plugins and a pointer identity for closure plugins (dynamic modules add their own key kind, §10).

### 8.2 Configuration (D7)
* Configuration arrives as `serde_json::Value` (from files, the loader API, or `plugin_value`) and is decoded **once** per (re)load attempt with `serde_path_to_error`, producing `ValidationError { issues: Vec<Issue { path, message }> }` – the analogue of Standard Schema issues. Unknown fields are tolerated unless the plugin opts into `deny_unknown_fields` (repository rule: tolerate unknown fields).
* `Fiber::update(value)`: validate first (a rejected config leaves the running instance untouched), emit `ConfigCommitted` (loader write-back) unless `no_save`, then offer the new config to the plugin's **update hook**; if no hook accepts it, `restart()` (`revision += 1` → new target → unload → reload). Hook registration is an effect: `ctx.on_config_update::<P>(|new| async { …; Ok(Handled::Yes) })`.

### 8.3 The lifecycle state machine (`lifecycle.rs`)
A faithful port of `_checkImpl` / `_refresh` / `_setEpoch` / `_reload` / `_unload` / `_updateState`, split into pure critical sections returning actions:

```rust
// critical section: recompute target (refresh)
fn refresh(st: &mut State, id: FiberId) -> Actions {
    let f = st.fiber(id);
    let target = st.compute_target(id);          // Inactive if removed || error || missing key; else Epoch{revision, providers}
    if target == f.target || f.error.is_some() { return Actions::none() }
    let old = replace(&mut st.fiber_mut(id).target, target.clone());
    if st.fiber(id).inertia.is_some() { return Actions::none() }        // inertial
    match (old, &target) {
        (Target::Inactive, Target::Epoch { .. }) => st.begin(id, Transition::Reload),   // L-Begin → Loading
        _                                         => st.begin(id, Transition::Unload),  // L-Leave → Unloading (+ notify consumers)
    }
}

// spawned task
async fn reload(root: Arc<Root>, id: FiberId) {
    let (start, apply_fut) = root.with(|st| st.commit_view(id));        // ω := resolved; register as dependent; snapshot target
    tokio::task::yield_now().await;                                     // coalesce superseded updates (TS: await Promise.resolve())
    let outcome = if root.with(|st| st.target_is(id, &start)) {
        AssertUnwindSafe(apply_fut).catch_unwind().await                // run apply; effects register themselves
    } else { Ok(Ok(())) };
    root.run(|st| st.finish_reload(id, &start, outcome));               // Active | Failed | chain into unload
}

async fn unload(root: Arc<Root>, id: FiberId) {
    root.wait_until(|st| st.provisions_free(id)).await;                 // fiber-level guard (relied_n)
    let disposers = root.with(|st| st.drain_disposables_rev(id));
    join_all(disposers.into_iter().map(run_logged)).await;              // concurrent, errors logged
    root.run(|st| st.finish_unload(id));                                // release providers, ω := None, Inactive | chain into reload
}
```
* `begin` sets the state, creates the `inertia` shared future, and – on leaving `Active` – emits notifications for every provision still bound (L-Leave before any inverse).
* `finish_reload`: if the target is unchanged → `Active` (or `Failed` if the apply errored; the error is recorded only if still current – a superseded failure only cleans up); otherwise chain into `unload`.
* `finish_unload`: drop self from each committed impl's `dependents` and wake providers whose set became empty; then `Inactive` (→ `Pending`/`Failed`/`Disposed` by `settled_state()`) or chain into `reload` if the target is no longer ⊥.
* `wait_until` uses `Notify` with `Notified::enable()` registered *before* releasing the lock (no lost wake-ups).

### 8.4 Instantiation (O-Insert / O-Retire)
`ctx.plugin(..)` creates `FiberData` (fresh `FiberId`, child `Scope`), resolves injections, and registers on the **parent** fiber an effect whose body inserts the fiber into its runtime and runs `refresh`, and whose inverse marks it removed, forces the target to ⊥, awaits the transition chain and settles to `Disposed` (TS `fiber.ts:178-206`). Unloading a parent therefore cascades to children through the parent's accumulator; re-enabling creates a *fresh* fiber (paper §4.4 Configuration).

### 8.5 Registry replace
`registry.replace(old, new)` (aliases + `_replace` on each fiber: new runtime, new inject, re-resolve, clear error, `revision += 1`, refresh) is ported as is; it is the primitive behind HMR and is useful on its own (swapping an implementation behind live fibers).

### 8.6 `Fiber` handle API
```rust
impl Fiber {
    pub fn id(&self) -> FiberId;  pub fn state(&self) -> FiberState;  pub fn context(&self) -> Context;
    pub async fn settled(&self) -> Result<(), Arc<PluginError>>;   // TS fiber.await()
    pub async fn dispose(&self);                                    // retire
    pub async fn restart(&self) -> Result<(), Arc<PluginError>>;
    pub async fn update(&self, config: serde_json::Value, no_save: bool) -> Result<(), UpdateError>;
    pub fn effects(&self) -> Vec<EffectMeta>;                      // introspection (labels, children)
}
```

---

## 9. Events, logger and built-in services

### 9.1 Typed events
```rust
pub trait Event: Send + Sync + 'static { type Output: Send = (); }
impl Context {
    pub fn on<E: Event>(&self, f: impl Fn(&E) -> E::Output + Send + Sync + 'static) -> EffectHandle;
    pub fn emit<E: Event>(&self, e: &E);
    pub async fn parallel<E: Event>(&self, e: &E) -> Result<(), AggregateError>;
    pub async fn serial<E: Event>(&self, e: &E) -> Option<E::Output>;   // first "bailed" result
    pub fn bail<E: Event>(&self, e: &E) -> Option<E::Output>;
}
```
Listeners are stored in an `IdTable` per event `TypeId`; registration is an effect of the listener's fiber; dispatch snapshots the table and skips removed listeners; optional realm filtering (`E: Scoped`) reproduces `Context.filter` for `ServiceChanged`. Waterfall-style extension points (`internal/config`, `internal/get`, `internal/update`) are replaced by **explicit hook traits** (config transform hook for the loader, update hook per fiber, commit hook) rather than stringly-typed events.

Internal events: `PluginInserted/Removed`, `StatusChanged { fiber, old, new }`, `ServiceChanged { key, realm }`, `ConfigCommitted { fiber, value }`, `Replaced { fiber }`.

### 9.2 Logger
`Logger` is a core service: `ctx.logger(name)` → `Logger { name, level, fiber }` (name defaults to the calling fiber's runtime name, overridable by interception metadata `{ name, level }`); `Exporter` trait objects registered via `logger.exporter(e)` **as effects**; ring buffer of the last 1000 `Message { sn, ts, name, level, args }`. Format strings use Rust formatting (`format_args!`) instead of printf directives. A `tracing` bridge (a `tracing_subscriber::Layer` feeding exporters) can be added in `cordis-logger-console`; it is not required by core.

### 9.3 Built-in plugin crates
| Crate | Port of | Rust building blocks |
|---|---|---|
| `cordis-timer` | `plugin-timer` | `tokio::time::{sleep, interval}`, `JoinHandle::abort` as disposers; `timeout(d) -> Result<(), Disposed>` future; `interval(d) -> impl Stream`; `throttle/debounce` handles |
| `cordis-logger-console` | `plugin-logger-console` | `anstream`/`anstyle` colours, label alignment, level map |
| `cordis-fs` | `plugin-fs-node` | `notify` (watch parent dir for single files), `globset` + ported last-match-wins/ancestor predicate |

---

## 10. Loader, include and dynamic modules

### 10.1 `cordis-loader`
Port of `packages/loader` with these Rust adaptations:
* **Data model**: `EntryOptions` as a serde struct with `#[serde(flatten)] extra: IndexMap<String, Value>` (tolerate unknown keys); `Option<Option<T>>`/`Value::Null` for "delete on merge"; `IsolateLabel::{Private, Shared(String)}`; `Disabled::{Bool, Expr}`.
* **Id-addressed documents instead of object identity**: TS relies on `entry.options === group.data[i]`; Rust keeps the group's `Vec<EntryOptions>` authoritative and entries hold ids; "came from the file" is a provenance flag, not `===`.
* **Entry state machine**: `update / _patchContext / _reconcile / init / _init / _start` ported 1:1 with the "re-check after every await" style (`fn still_current(&self, snap) -> bool`), `_initTask` as `Option<Shared<..>>`.
* **Two fibers per entry with `inject`** (outer dependency holder + inner plugin) kept: the outer `apply` resolves the plugin from the catalog, hits `ctx.checkpoint().await?`, then starts the inner fiber.
* **Catalog** replaces module import: `Catalog::register::<P>()` / `register_alias("@cordisjs/plugin-group", Group)`; a missing name yields `ModuleNotFound` located at the entry's `name`.
* **Managed realms**: `LocalRealm`/`GlobalRealm` as `RealmId`s with labels; the 7-step `patch-context` algorithm ported faithfully, with the delimiter test computed directly by walking scope parents (`scope.is_within(entry_marker)`) instead of temporary symbols; provider migration without restart; blocked moves tracked in `Impl::realm`. **Fix** the realm-GC early `return` (use `continue`).
* **Self-dispose / self-update write-back**: core passes a `DisposeReason { SelfRequested, Parent, Loader }` instead of the TS `_disposing` flag + unregister-before-dispose trick (a deliberate breaking simplification).
* **`loader` await barrier**: `Loader::settled().await` and an availability check driven by an atomic task counter.
* **Validation**: `validate_options(&Value, deep) -> Vec<Issue>` at the boundary (deep for API calls, shallow for file input).
* **Expressions** (`!!js`): JS `with(ctx) eval` cannot be ported. Decision point (§13): adopt a side-effect-free expression language (recommended: CEL, or a small custom evaluator) whose identifiers resolve through `ctx.get_dyn(name)` and a serializable service view (`trait ExprValue { fn to_value(&self) -> Value }`), respecting the same evaluation-context table as the TS loader. Until then, `Expr` values are rejected with a clear validation issue.

### 10.2 `cordis-include`
* Pure modules ported almost verbatim and unit-tested in isolation: `journal` (record, merge, flatten, apply, three-way `reconcile` – file wins), `patch` (insert/override patches, `PatchIndex` ownership, `route_journal`), anonymous id assignment.
* IO loop: one task driven by `dirty_read/dirty_write` flags + `Notify`; latest-wins tree application; `flush()`.
* Parsing: a YAML parser with spans and custom tags (`saphyr` family) – `!!js` → `Value::Expr`; errors **and** warnings fatal; JSON via `serde_json` plus a spanned pass for locations.
* Atomic writes: temp file in the same directory, stale-content check before every rename, bounded retry on `PermissionDenied`/`ResourceBusy`, read-only → keep journal in memory.
* Errors: structured `ConfigFrame { id, file, line, col }` chains instead of rewritten stack strings.

### 10.3 Dynamic modules and HMR (`cordis-hmr`, optional, Phase 7)
Paper §6.4: *native code exposes no module registry, so introduction and retraction take the form of explicit dynamic linking (dlopen/dlclose) … WebAssembly … released when a native embedder (e.g. Wasmtime) drops it.* The plan separates the **algorithm** (portable) from the **module technology** (a backend):

```rust
pub trait ModuleSource: Send + Sync {
    fn load(&self, id: &ModuleId) -> Result<LoadedModule, LoadError>;   // exports: Vec<PluginDef>, deps: Vec<ModuleId>
}
```
* The host owns a `ModuleGraph { records: HashMap<ModuleId, ModuleRecord { deps, kind: Host|Dynamic, instance } > }` replacing Node's caches.
* The Cordis reconcile algorithm (match by path ancestry → reverse-dependency acceptance → group by runtime → stage 1 load+validate everything → union-find units over old ∪ new closures → restore aborted units → stage 2 `registry.replace` in place → stage 3 settle concurrently; serialized transactions, re-entrancy rejected via a task-local transaction id) is ported as pure graph code with tests from `reconcile.spec.ts`. This follows the **code**, not the paper's Algorithm 10 (see [`implementation/09-hmr.md`](implementation/09-hmr.md#6-paper-vs-code)).
* Backends, in order of preference:
  1. **WASM components (wasmtime)** – old and new instances coexist safely, dropping is sound, natural sandbox (paper §6.3 bridge fibers). Cost: services cross a WIT boundary.
  2. **dylibs (`libloading`)** – development-time only: same toolchain, each version loaded from a unique copied path, **never unloaded** (unloading Rust dylibs is unsound with TLS destructors and leaked `'static` references), C-ABI entry point.
  3. Out-of-process plugins (respawn = reload) via the service-broker/RPC pattern of paper §6.2.
* Statically linked plugins are "host modules" by construction; changing them requires a process restart, while live *re*configuration remains fully supported by the loader.

---

## 11. Testing strategy

### 11.1 Port the TypeScript suite (acceptance)
Each phase ports the corresponding `*.spec.ts` files as Rust integration tests on a `current_thread` runtime with paused time (`#[tokio::test(start_paused = true)]`), preserving test names for traceability:

| Phase | Specs to port |
|---|---|
| 1 | `dispose.spec` (effect shapes, abort at boundaries, finally/cleanup, errors) |
| 2 | `fiber.spec` (inertia locks 1–3, failure, update, dispose of pending/failed), `update.spec` (concurrent updates), `service.spec`, `service-dispose.spec` (guards, drain ordering, concurrent sibling cleanup), `plugin.spec`, `replace.spec`, `isolate.spec`, `reflect.spec` |
| 3 | `events.spec`, `intercept.spec`, `logger.spec`, `shadow.spec` (re-expressed as def-site/use-site tests on `Svc`), timer `index.spec` |
| 4 | loader `validate.spec`, `commit.spec`, `group.spec`, `index.spec`, `evaluate.spec`, `rebuild.spec`, `isolate*.spec` |
| 5 | include `sync.spec`, `patch.spec`, `reload.spec`, `write.spec` (fault injection on rename), `parse.spec`; fs `watch.spec` |
| 7 | hmr `reconcile.spec` (with an in-memory `ModuleSource`) |

### 11.2 Model-based tests of the paper's metatheory
Because the lifecycle is written as pure critical sections (§4.2), the core can be driven by a **deterministic scheduler** in tests: a harness that holds spawned transitions in a queue and lets `proptest` choose which runnable task advances next. With synthetic components whose effects are writes to a shared table (each with an exact inverse), random orchestration sequences (insert / retire / provide / withdraw / update) and random schedules check:
* **Preservation** (Thm 64): registry well-formedness after every step (parents exist, provisions disjoint per realm, committed views name installed fibers).
* **Recovery exactness** (Thm 68 / Cor 69): after a fiber's episode closes, the table equals the run with that fiber's steps removed; a removed fiber's provisions are empty.
* **Ordering** (Thm 70): a provider's episode strictly contains each consumer's episode that committed to it; no consumer reads a binding outside its provider's episode.
* **Resolution coherence** (Thm 71): every `apply` observes one committed view.
* **Progress** (Thm 73): every schedule reaches quiescence within the step bound (acyclic declarations).
* **Confluence** (Thm 80): for fixed orchestration inputs, all schedules reach the same set of `Active` fibers (= the support set A) and the same table up to fiber renaming.
These tests check the implementation against the theory with no business logic copied into the tests.

### 11.3 Unit tests
Pure modules (`IdTable`, `Scope` lookups, target computation, `journal`, `patch`, glob predicate, HMR union-find) get focused unit tests. Mocks are limited to `Catalog`/`ModuleSource` test registries and fault-injecting filesystem shims.

### 11.4 Tooling
`cargo fmt`, `cargo clippy -D warnings`, `cargo test --workspace`; `cargo miri` is not needed (no `unsafe`); optional `loom` is *not* planned (the single-lock design has no lock-free state to model).

---

## 12. Implementation phases

| Phase | Scope | Exit criteria |
|---|---|---|
| **0. Scaffold** | Workspace, crates, CI (fmt, clippy, test) | empty crates build |
| **1. Effects & store** | ids, `Root`/`Context`/`Scope`, `IdTable`, effect engine (`effect_sync`, `effect`, `EffectScope`, `EffectHandle`), `provide`/`try_get_live`, isolate/intercept overlays; root fiber only | `dispose.spec` ported and green |
| **2. Fibers & lifecycle** | `Plugin` trait, registry/runtimes, `FiberData`, target/committed views, refresh/reload/unload with inertia, two-level guard, failure, `update`/`restart`, instantiation cascade, `replace`, checked `get` (Algorithm 6), cycle warning | Phase-2 specs green; model-based property tests (§11.2) green |
| **3. Services & built-ins** | `Service`/`Svc<S>` caller attribution, interception merge, typed events + internal events, logger + exporters, `cordis-timer`, `cordis-logger-console` | Phase-3 specs green; example app with a provider/consumer pair and hot provider replacement |
| **4. Loader** | `EntryOptions`, validation, `EntryTree`/`EntryGroup`/`Group`, entry state machine, outer/inner fibers, catalog, managed realms + migration, write-back hooks, `loader` barrier; `cordis` binary bootstrapping a root with the loader | loader specs (minus expressions) green |
| **5. Include & fs** | `cordis-fs` watcher; `cordis-include` parse/journal/patch/sync/atomic write/locations; `cordis.yml` bootstrap identical to `core/bin.js` | include + fs specs green; editing `cordis.yml` live reconfigures a running app |
| **6. Ergonomics** | `cordis-macros`: `#[plugin]`/`#[service]` derive (NAME, inject tuples, extension traits), compile-time undeclared-access errors for typed deps; optional expression language | macro tests (trybuild) |
| **7. Dynamic modules (optional)** | `ModuleSource`, `ModuleGraph`, ported HMR reconcile; WASM backend prototype; dylib dev backend | `reconcile.spec` ported against an in-memory source |

Phases 1–3 deliver the paper's core library; 4–5 the paper's loader; 7 the paper's HMR.

---

## 13. Risks and open decisions

| Topic | Risk / question | Proposed answer |
|---|---|---|
| Threading model | `Send` bounds everywhere vs. a literal JS-like `!Send` design | `Send + Sync` with the lock-plus-actions rule (§4.2); revisit only with evidence |
| Reentrancy | user code called under the state lock deadlocks (`parking_lot` is not reentrant) | enforced by structure: critical sections return `Actions`; availability `check` closures documented as runtime-free |
| Disposal order | paper LIFO vs. Cordis concurrent fiber-level disposal | follow Cordis (tests depend on it); LIFO within one effect |
| Cancellation | dropping `apply` futures would leak half-registered resources | inertial host + explicit checkpoints (§7.3) |
| Reference cycles | services hold their own `Context` (→ `Root`), `Root` holds services | cycles are broken when bindings are withdrawn; `Root::shutdown().await` disposes the root fiber and clears the store |
| Expressions | no JS `eval` | decide on CEL vs. custom evaluator before Phase 4 finishes; reject expressions until then |
| Dynamic code | Rust has no module registry; dylib unloading unsound | static catalog first; WASM backend for real hot reload/sandboxing |
| Type-level keys | string keys can collide; `TypeId` unstable across dylibs | `TypeId` check in-process; string + version tag for dynamic modules (paper §6.6) |
| Error-message parity | TS messages/columns (UTF-16) | keep message wording where cheap; no UTF-16 parity |

---

## 14. Worked example (target API)

```rust
use cordis::prelude::*;

struct Database { url: String }
impl Service for Database { const NAME: &'static str = "database"; }

struct DbPlugin;
#[derive(Deserialize)] struct DbConfig { url: String }
impl Plugin for DbPlugin {
    const NAME: &'static str = "db";
    type Config = DbConfig;
    type Inject = ();                                        // explicit: associated-type defaults are unstable
    fn provide() -> &'static [&'static str] { &["database"] }
    async fn apply(ctx: Context, cfg: Arc<DbConfig>) -> Result<(), PluginError> {
        let pool = connect(&cfg.url).await?;                 // acquisition…
        ctx.on_dispose({ let p = pool.clone(); move || p.close() });   // …paired with its inverse
        ctx.checkpoint().await?;                             // iteration boundary
        ctx.provide(Database { url: cfg.url.clone() })?;     // tracked provision
        Ok(())
    }
}

struct Greeter;
impl Plugin for Greeter {
    const NAME: &'static str = "greeter";
    type Config = ();
    type Inject = (Database,);                               // d = { database }
    async fn apply(ctx: Context, _: Arc<()>) -> Result<(), PluginError> {
        let (db,) = ctx.deps::<Self::Inject>()?;             // reads the committed view
        assert!(ctx.get::<Timer>().is_err());                // undeclared → Err(Undeclared) unless inherited
        ctx.logger("greeter").info(format_args!("using {}", db.url));
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = Root::new();
    let ctx = root.context();
    let greeter = ctx.plugin::<Greeter>(())?;                // Pending: database not provided yet
    let db = ctx.plugin::<DbPlugin>(DbConfig { url: "postgres://…".into() })?;
    greeter.settled().await?;                                // Active once db is Active
    db.update(json!({ "url": "postgres://other" }), false).await?;   // greeter unloads first, db reloads, greeter reloads
    db.dispose().await;                                      // greeter back to Pending; all effects reverted
    root.shutdown().await;
    Ok(())
}
```

---

## 15. Implementation status

All phases of §12 are implemented in `crates/` (`cargo test --workspace`; `cargo run -p cordis -- examples/cordis.yml` boots an app from a watched config file). The specs listed in §11.1 are ported except the Node-specific ones (`resolve`, `import`, module-cache details of `reconcile`, `logger-console` formatter directives, `create`, `package-manager`).

Where the implementation departs from this plan or from the TypeScript code:

| Topic | Implementation |
|---|---|
| Retired providers | Found by the §11.2 property tests: retiring a fiber unbinds its live provisions at once, so a successor can provide in the same realm while old consumers still drain (they read through their committed views). The TS code keeps the slot until the drain and fails the successor. |
| Stale loads | A retired fiber's load still in flight may register inverses (so nothing it acquired leaks), but not provisions. |
| Provision removal during a drain | Postponed to the end of the provider's drain, so descendants can reach inherited services during their own cleanup without relying on microtask order. |
| Re-pointable contexts | Scope nodes carry their fiber; the fiber chain of an access is read off the derivation chain, so moving an entry also moves the fibers below it (the TS prototype behaviour). Entry tables are mutable `Overlay` nodes. |
| Checked typed access | `Injected<D>::get::<S, _>()` (the `_` is the inferred position of `S` in `D`); an undeclared `S` does not compile. |
| Expressions | A side-effect-free JavaScript subset (`cordis_loader::expr`) over JSON views of services; no function calls. |
| Errors | Parse and validation errors of config files carry `file:line:col`; validation errors of plugin configs are not yet relocated to file positions, and `Entry::evaluate` is not exposed. |
| Loader API | Resolving a parent group waits for a group entry whose plugin is still loading (its group exists only once its `apply` runs). |
| Dynamic modules | `ModuleSource` / `ModuleGraph` / `Hmr`, plus two file backends that share one JSON guest ABI (documented in the `cordis-hmr` crate docs) and a bridge plugin instantiating the guest per fiber. `WasmSource` runs core WASM modules on wasmtime, one store per fiber, instead of WIT components, so guests need no component toolchain (the tests are written in WAT). `DylibSource` speaks a C ABI instead of passing Rust types: a cdylib carries its own copies of `cordis-core` and tokio, with separate statics and no runtime context. Each version is loaded from a unique copy and never unloaded. Guest services are JSON views with JSON calls (`GuestService`). `cordis-guest` is the Rust guest SDK: a `Guest` trait and `export!`, which emits the WASM exports on wasm32 and the C symbols elsewhere, so one source builds for both backends. Watch mode (`HmrConfig { base, watch: { enabled, files, debounce } }`) follows the TS code: it watches `base` through `fs` and reloads each debounce window as one batch, one batch at a time. The files of failed imports are retried with the next batch, and changes to host modules are only reported. `base` defaults to the working directory, since Rust contexts carry no `baseUrl`. |
