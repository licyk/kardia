# Implementation as Described in the Paper (Paper §5)

> **Layer:** Theory → design. This document records what the *paper* says Cordis does (Algorithms 1–10, Table 2). The actual TypeScript code differs in several details; those differences are catalogued in [`../cordis/11-paper-vs-implementation.md`](../implementation/11-paper-vs-implementation.md).

Cordis is a **meta-framework**: it prescribes no domain (no routing, ORM, UI); its only responsibility is universal dynamic-composition semantics. Three tiers:
1. **core library** – effect & coeffect systems (§5.1);
2. **component loader** – configuration reconciliation and HMR (§5.2);
3. **application frameworks** – e.g. Koishi (§5.3).

## 1. Table 2 – theory ↔ runtime

| Theory | Runtime |
|---|---|
| Γ∞ | `ctx`, the first-class context |
| γ ∈ Γ | the context tree plus everything the running system touched |
| 𝔈Γ, ℑΓ | effect callback returning / yielding inverses |
| effect(e) | `ctx.effect(callback)` |
| Σ, Σ_iso, Σ_inter | `ctx[@@store]`, `ctx[@@isolate]`, `ctx[@@intercept]` (symbol-keyed slots) |
| get / set | `ctx.get(key)`, `ctx.set(key, value)` |
| isolate / intercept | `ctx.isolate(key, realm)`, `ctx.intercept(key, metadata)` |
| fiber ⟨d,p,e,π,σ,τ,θ⟩ | `fiber` |
| dom(F) | enumerated through `ctx.registry` |
| name n | `fiber.uid` |
| d | `fiber.inject` |
| p | the component's `provide` |
| e | `fiber.apply` |
| π | `fiber.parent.fiber.uid` |
| derived realization | `fiber.ctx`, the child context the fiber runs in |
| θ | `fiber.state` (`LOADING` = Reloading; `FAILED` carries the error) |
| accumulator g / recover | `fiber.dispose` |
| ω | `fiber.committed` |
| provider_k | an `Impl` whose provider fiber is `ACTIVE` |
| target | `fiber.target`, recomputed by `refresh`; ⊥ is `INACTIVE` |
| Future / inertia | `fiber.inertia`, handle of the transition in flight |
| O-Insert, O-Retire | `ctx.use` and the inverse of its callback |
| O-Remove | fiber dropped from its runtime, uid cleared |
| L-Begin/Iter/Finish | `execute`'s iteration loop |
| L-Divert | guard failing at an iteration boundary, or reload chaining into unload |
| L-Leave | `refresh` marking the fiber `UNLOADING` |
| L-Unload | `unload` and its inertial chaining |
| guard | `unload` awaiting notified dependents |
| failure | error recorded on the fiber, target set to ⊥ |

## 2. Effect tracking (§5.1.1, Algorithm 1)

Every context mutation – provision, instantiation, listeners, … – reduces to one primitive, `ctx.effect`, so any operation performed through the context is automatically tracked.

```
async function execute(callback, guard)
    iter ← callback(); inverse ← id
    while guard()
        (value, done) ← await iter.next()
        if value then inverse ← value ∘ inverse     // prepend ⇒ LIFO
        if done then break
    return inverse

function effect(ctx, callback)
    armed ← true
    task ← execute(callback, () ↦ armed)
    async function dispose()
        if not armed then return
        armed ← false            // halts in-flight iteration; at-most-once
        recover ← await task
        recover()
    ctx.dispose ← dispose ∘ ctx.dispose    // parent composition (∂²Γ)
    return dispose
```
* Accepts both effect functions and iterators (ad-hoc polymorphism); the guard is checked at each iteration boundary.
* **Not checked:** the witness (that the inverse really reverts) and the coeffect commutativity witness – author/provider obligations.
* The component level reuses `execute` with a guard testing the stability of `fiber.target`.

## 3. Coeffect operations (§5.1.2, Algorithms 2–3)

Each context carries three symbol-keyed slots: `@@store` (realm symbol → value), `@@isolate` (key → realm symbol), `@@intercept` (key → metadata).

```
get(ctx, key):   realm ← ctx[@@isolate][key];  return ctx[@@store][realm]
set(ctx, key, v): ctx.effect(() ⇒ {
                    realm ← ctx[@@isolate][key]; ctx[@@store][realm] ← v; notify(ctx, [key])
                    return () ⇒ { delete ctx[@@store][realm]; notify(ctx, [key]) } })

notify(ctx, keys):
    for fiber in all_fibers
        for key in keys
            if key ∈ fiber.inject and fiber.ctx[@@isolate][key] = ctx[@@isolate][key]
                refresh(fiber); affected ∪= {fiber}; break
    return affected
```
* A binding counts as available only while the installing fiber is `ACTIVE`; a provider entering `UNLOADING` stops providing, so dependents recompute an unsatisfied target and begin their own teardown *while its bindings are still in place*.
* `isolate` / `intercept` derive a child context overriding one inherited table; recovery is implicit (discard the child). `isolate` defaults to a freshly generated symbol; `intercept` merges new metadata over the inherited value (new wins).

## 4. Component lifecycle (§5.1.3, Algorithms 4–5)

```
function use(ctx, component, config)
    function callback()
        refresh(fiber)
        return () ⇒ { fiber.target ← ⊥; unload(fiber) }     // O-Retire
    fiber ← Fiber(parent: ctx, inject: component.inject)
    fiber.ctx ← ctx[fiber ↦ fiber]
    fiber.apply ← () ↦ component.apply(fiber.ctx, config)  // config bound in
    ctx.effect(callback)                                    // O-Insert as a tracked effect
    return fiber

function refresh(fiber)
    target ← target(γ, n)
    if target = fiber.target then return
    fiber.target ← target
    if fiber.inertia then return                          // transition in flight: inertial
    if target ≠ ⊥ then fiber.state ← LOADING;   fiber.inertia ← task(reload(fiber))
    else               fiber.state ← UNLOADING; fiber.inertia ← task(unload(fiber))   // L-Leave

async function reload(fiber)
    target0 ← fiber.target
    fiber.committed ← resolve(fiber.inject)               // commit the view
    recover ← await execute(fiber.apply, () ↦ fiber.target = target0)
    fiber.dispose ← recover ∘ fiber.dispose
    if fiber.target = target0 then
        fiber.state ← ACTIVE; notify(fiber.ctx, provided(fiber)); fiber.inertia ← null
    else
        fiber.state ← UNLOADING; fiber.inertia ← task(unload(fiber))

async function unload(fiber)
    await all(notify(fiber.ctx, provided(fiber)).map(f ↦ f.await()))   // the guard: drain dependents
    await fiber.dispose(); fiber.dispose ← id; fiber.committed ← ⊥
    if fiber.target = ⊥ then fiber.state ← INACTIVE; fiber.inertia ← null
    else fiber.state ← LOADING; fiber.inertia ← task(reload(fiber))
```
Footnote 2 explicitly addresses Rust: *"with lazy scheduling (e.g. Python coroutines, Rust futures) the host must spawn the task for it to progress"* – `create_task` is written out for language independence.

* `fiber.target` is a digest tupling the uid of each declared key's provider. A **uid is never reused**, so a replaced provider is never mistaken for the old one even if values are equal. Consequently a provider overwriting its own binding *in place* is not observed – to propagate a replacement, withdraw and re-install.
* Two levels of checking: transition level (reload/unload check the target at completion → inertial chaining) and iteration level (execute checks at every boundary → partial rollback).
* Three lines carry Theorem 70: commit at reload start, discard committed only after every inverse; mark `UNLOADING` before scheduling the task; `unload` waits for each notified dependent. The wait sits **before** the whole recovery, since `fiber.dispose` initiates a fiber's effects concurrently.
* Termination follows Theorem 73: the provider graph is traversed on demand.

## 5. Context access (§5.1.4, Algorithm 6)

Besides reflective `ctx.get/set`, coeffects are read as **properties** (`ctx[key]`) via a JS `Proxy`:
```
function resolve(ctx, key)
    fiber ← ctx.fiber
    repeat
        if key ∈ fiber.committed then return fiber.committed[key]
        if key ∈ fiber.inject then throw INACTIVE_ACCESS
        if fiber = root then throw UNDECLARED_ACCESS
        fiber ← fiber.parent.fiber
```
Unlike `ctx.get` (never fails), the proxy resolves against the accessing fiber's **committed view** and enforces the specification at the point of use. Reading the view rather than the store is what keeps a dependency readable during the consumer's own teardown (Theorem 70). The check is at runtime but, since `inject` is static, could be done at compile time (§6.4 – e.g. Rust proc macros / traits).

## 6. Component loader (§5.2)

### Declarative configuration (§5.2.1)
**Definition 81 – entry:** `id` (stable reconciliation key), `url` (component module), `isolate`, `intercept`, `config`, `disabled`. An entry is a faithful specification because it records exactly what the support set reads: `disabled`→τ, tree parent→π, `url`→(d, p). Entries form a **configuration tree**; `@cordisjs/group` loads a list of child entries; `@cordisjs/include` grafts an external YAML/JSON file. Binding is **bidirectional**: a component that revises its own config or disables itself has the change written back.

**Reconciliation** is incremental, justified by: Theorem 80 (endpoint is a function of the final config), Theorem 73 (it quiesces), Corollary 69 (rebuilding one entry leaves neighbours untouched), Theorem 70 (entries can be instantiated together; a fiber waits at L-Begin – so **modules are loaded concurrently**). Per-field dispatch:

| field changed | action |
|---|---|
| `id`, `url` | rebuild the entry |
| `isolate` | reassign realms (Algorithm 7) |
| `intercept` | update in place (consulted at read time; no reload) |
| `config` | hand to the component; typically diff and reload only on material change. A group's config is its child list → keyed diff over child ids, recursing |
| `disabled` | unload when set, reload when cleared |

**Managed realms.** `isolate: true` → *local* realm private to the entry, tagged by its id, carried along when the entry moves; `isolate: "name"` → *global* realm shared by every entry naming that string. A realm is discarded once no entry names it.

**Algorithm 7 – realm reassignment with delimiters.** One delimiter symbol δ_k per key; each context stores a tag under δ_k, inherited by descendants and redrawn on each reassignment, so `γ′[δ_k] = d₁ ⇔ γ′ derives from the entry's context`.
```
patch_isolation(entry, ρ′):
    ρ ← entry.ctx[@@isolate]; store ← entry.ctx[@@store]
    Δ ← {k | ρ(k) ≠ ρ′(k)}
    for k in Δ: entry.ctx[δ_k] ← fresh tag
                diff[k] ← (ρ(k), ρ′(k), entry.ctx[δ_k], store[ρ(k)].fiber.ctx[δ_k])
    entry.ctx[@@isolate] ← ρ′
    reload(entry.fiber)
    for k in Δ: (s1,s2,d1,d2) ← diff[k]
                if d1 = d2 and store[s1] and not store[s2]:     // binding is the entry's own
                    store[s2] ← store[s1]; delete store[s1]
    affected(fiber, k) := fiber.ctx[@@isolate][k] ∈ {s1,s2} and (fiber.ctx[δ_k] = d1) ≠ (d2 = d1)
    notify(entry.ctx, Δ, affected)
```
A dependent gains/loses the binding exactly when the move separates it from the provider.

### Hot module replacement (§5.2.2)
Because a fiber bounds all of a component's effects, a module that is a component can be replaced through fiber operations alone – **no developer-annotated acceptance boundaries** (unlike Webpack/Vite).

* **Phase 1 – classification (Algorithm 8):** inputs `stashed` (changed files) and `externals` (non-replaceable modules → full restart). Fixed point over `get_imports`: a pending module is *accepted* if one of its imports is accepted, *declined* if all imports are declined; leftovers (cycles) default to declined.
* **Phase 2 – stale-entry detection (Algorithm 9):** for each entry, collect transitive imports of `entry.url` stopping at declined modules; if the tree meets `accepted`, the entry is stale and its tree is folded into `accepted`.
* **Phase 3 – transactional reload (Algorithm 10):** invalidate caches of accepted modules (backing them up), re-import each stale entry and swap in a fresh fiber; on any error, restore caches and rebuild every stale entry from the backup, then rethrow. The system never enters a half-reloaded state. (On Node.js both ESM and CJS caches are cleared.)

## 7. Case study: Koishi (§5.3)

* Chatbot framework, 4+ years, >4000 community plugins (IM adapters, DB drivers, consoles, features); the web console is a second, independent Cordis application in the browser → the model is **expressive** (sufficient for a production system) and **general** (no domain or runtime presupposed).
* Disabling a plugin from the console reverts its effects in place; HMR re-applies edited plugins on save while preserving caches/connections elsewhere. Authors get ordered cleanup without writing an uninstall path.
* Genuine dependency topology (adapters, databases, features). Reconfiguring a provider reactivates only the dependents whose resolved dependency changed; a plugin with an unavailable dependency waits without erroring.
* **Threats to validity:** single ecosystem, single language, observational; overhead/productivity measurement is future work.
