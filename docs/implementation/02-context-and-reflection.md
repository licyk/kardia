# Cordis Core: Context, Reflection and Coeffect Resolution

> **Layer:** Implementation (TypeScript, `cordis/packages/core/src/context.ts`, `reflect.ts`, `utils.ts`).
> Paper counterpart: §3.2 (coeffects), §5.1.2 (Algorithms 2–3), §5.1.4 (Algorithm 6). See [`../paper/03-reactive-coeffects.md`](../paper/03-reactive-coeffects.md).

## 1. `Context` – the first-class context (`context.ts`)

```ts
export interface Context {
  [symbols.root]: this                // the root context (proxied)
  [symbols.isolate]: Dict<symbol>     // ρ: key → realm symbol (prototype-chained)
  [symbols.intercept]: Dict           // ι: key → metadata (prototype-chained)
  baseUrl?: string
  events: EventsService; logger: LoggerService; reflect: ReflectService; registry: RegistryService
  fiber: Fiber                        // (declared in fiber.ts)
}
```

### Construction (`context.ts:36-49`)
`new Context()` builds the **root**:
1. creates empty `isolate`/`intercept` tables (`Object.create(null)`);
2. wraps itself in a `Proxy` with `ReflectService.handler` – this proxy *is* the context everybody sees, `this[symbols.root] = self`;
3. creates the **root fiber** `new Fiber(self, {}, {}, null, …)` (runtime `null` ⇒ root, uid 0, `ACTIVE`);
4. instantiates the four built-in services on the root: `reflect`, `registry`, `events`, `logger`;
5. `this.fiber._disposables.clear()` – the built-in mixins/accessors registered during construction are *not* part of the root fiber's revertible effects (they must survive `root.fiber.dispose()`).

### Derivation: `extend()` (`context.ts:55-63`)
```ts
extend(meta = {}) {
  const self = Object.create(getTraceable(this, this))   // prototype-chain child
  // copy own descriptors of meta onto self
  // preserve a shadow wrapper if `this` is a shadow context
}
```
Every derived context is a **prototype-chain child** of its parent. This realizes the paper's *derived realization* (Definition 23): the child overrides some slots and inherits everything else; nothing needs to be undone – dropping the child object discards the adjustment.

* `isolate(name, label?)` (`context.ts:65`) – creates `Object.create(this[symbols.isolate])`, sets `shadow[name] = label ?? Symbol(name)` and extends with it. A fresh symbol ⇒ a private realm; a shared `label` ⇒ a shared realm.
* `intercept(name, config)` (`context.ts:73`) – same pattern for the intercept table.
* A fiber's context is `parent.extend({ fiber: this })` (`fiber.ts:150`).

**Key property:** isolation/interception tables are *prototype chains*. Lookups (`table[name]`) see the nearest override; `Object.hasOwn(table, name)` detects an isolation boundary introduced at exactly that level (used by `locate`, below).

### Realm symbols
`ReflectService.provide` lazily assigns a root realm: `ctx[symbols.root][symbols.isolate][name] ??= Symbol(name)` (`reflect.ts:222`). Thus *every* service name has a realm symbol in the root table once provided; isolated contexts override it. The **store is keyed by realm symbol**, giving the two-layer `k → ρ(k) → σ(ρ(k))` resolution of Definition 24.

## 2. `ReflectService` (`reflect.ts`)

Holds the shared, root-level tables:

| Field | Paper | Meaning |
|---|---|---|
| `store: Dict<Impl, symbol>` | σ (`@@store`) | realm symbol → current `Impl` (binding) |
| `props: Dict<Property>` | – | declared context properties: `{type:'service'}` or `{type:'accessor', get, set}` |
| `_implCounter` | fresh names | uid source for `Impl`s (never reused) |

### `Impl` – a binding (`reflect.ts:65-75`)
```ts
interface Impl {
  uid: number            // fresh per provision ⇒ provider identity for target views
  name: string
  ctx: Context           // provider's context (impl.ctx.fiber = provider fiber)
  dependents: Set<Fiber> // fibers whose committed view references this impl
  release?: () => void   // resolver for the provider-side guard
  value?: any
  check?: () => boolean  // Service[symbols.check] – extra availability predicate
  isolate: symbol        // actual realm symbol (may differ from provider ctx after a blocked move)
}
```

### `provide(name, value?, check?)` – coeffect provision (`reflect.ts:213-249`)
A `fiber.effect` (therefore tracked and revertible) whose callback:
1. registers/validates `props[name]` as a service (error if already an accessor);
2. ensures a root realm symbol; computes `key = ctx[isolate][name]`;
3. **single-source check**: throws `service "x" has been registered at <fiber>` if `store[key]` exists (O-Insert's disjointness, per realm);
4. `store[key] = impl`, `fiber.provisions.add(impl)`;
5. if the providing fiber is already `ACTIVE`, `notify([name])` immediately; otherwise notification happens when the fiber becomes `ACTIVE` (`Fiber._updateState`), matching "only Active fibers provide".

Its **inverse** (async):
1. delete the store entry (if it still holds this impl), `notify` dependents in the impl's realm;
2. **provider-side guard**: `await` until `impl.dependents` is empty (`impl.release` is resolved by the last dependent's unload); it always yields at least once so child cleanup can still access the inherited service;
3. remove the impl from `fiber.provisions`.

### `set(name, value)` (`reflect.ts:200-211`)
In-place overwrite of the *own* binding (`impl.value = value`); errors if not provided or provided by another fiber. **No notification** – as the paper notes, an in-place overwrite is not observed by dependents.

### `get(name, strict = true)` / `_getImpl` (`reflect.ts:188-198`)
Resolves `ctx[isolate][name]` → `store[key]`; in `strict` mode returns nothing unless `impl.ctx.fiber.state === ACTIVE` (**provider availability = Active**). Never throws.

### `notify(names, filter?)` – reactive notification (`reflect.ts:251-270`)
```ts
for runtime of registry.values()
  for fiber of runtime.fibers
     for name of names
       if name ∈ fiber.inject && filter(fiber.ctx, name):   // default: same realm symbol
         fiber._checkImpl(name)                             // recompute this key's resolution
     if any: fiber._refresh()                                // recompute target, maybe transition
emit 'internal/service'(name, value) with a realm filter on listeners
```
This is Algorithm 3. The *filter* parameter exists so the loader can replace the realm test during realm reassignment (Algorithm 7).

### `accessor(name, {get, set})` and `mixin(source, keys)` (`reflect.ts:272-308`)
* `accessor` declares a computed context property (as a revertible effect).
* `mixin('events', ['on', 'emit', …])` exposes members of a service as context properties (`ctx.on` → `ctx.events.on`) bound to the accessing context; it is a generator effect that yields one accessor per key. The root wires: `reflect: get,set,provide,accessor,mixin`; `fiber: runtime,effect`; `registry: inject,plugin`; `events: on,once,parallel,emit,serial,bail,waterfall` (`reflect.ts:182-185`).

## 3. Property access through the Proxy (`ReflectService.handler`, `reflect.ts:105-170`)

`ctx.foo` is mediated by the `get` trap:
1. *special* properties (symbols, `prototype`, `then`, numeric strings, names starting with `_`) bypass mediation;
2. ordinary own/inherited properties → returned through `getTraceable` (see services doc);
3. a declared **accessor** → its getter;
4. if the *def site* (shadow, see below) is the **root fiber** → unchecked `reflect.get(prop, false)` (root code cannot declare `inject`);
5. otherwise a `waterfall('internal/get', …)` (lets plugins hook access) around `locate(defSite, prop)`:
   * found impl → value (traced);
   * fiber declares it but has no committed impl → `cannot get required service "x" in inactive context`;
   * nobody declares it → `cannot get property "x" without inject`.

The `set` trap: undeclared property on a non-root fiber → error `cannot set property without provide`; accessor → setter; service → `waterfall('internal/set')` → `reflect.set`.

Errors are created as cheap placeholders and only materialized with a trimmed stack when thrown (`createAccessError`/`enhanceAccessError`).

### `locate(ctx, name)` – the committed-view walk (`reflect.ts:83-102`)
Algorithm 6, extended with own provisions and isolation boundaries:
```
key ← ctx[isolate][name]; table ← ctx[isolate]; fiber ← ctx.fiber
loop
  if fiber provides `name` in the same realm  → { impl }          (own provision)
  walk `table` up to fiber.ctx[isolate]; if an own override of `name` is met → undefined
                                                                     (isolation boundary)
  if fiber.committed[name] or name ∈ fiber.inject → { fiber, impl }  (impl may be undefined ⇒ inactive)
  if fiber is root → undefined
  fiber ← fiber.parent.fiber
```
Consequences (all covered by `tests/isolate.spec.ts`, `reflect.spec.ts`):
* a consumer reads the **committed** impl, not the live store – it keeps seeing its dependency during its own teardown (Theorem 70);
* a child plugin *inherits* the injection of an ancestor (`inject` declared on an ancestor admits access in descendants);
* an isolation boundary stops inheritance (`does not inherit the injected view across isolation boundaries`).

## 4. Interception (`Service[symbols.resolveIntercept]`, `service.ts:135-148`)

Metadata for a service named `name` is resolved at access time:
1. walk the context's intercept prototype chain, collecting own values **outer → inner**;
2. prepend the *component-declared* metadata: `locate(caller, name).fiber.inject[name]` (the value given in `inject: { name: meta }` of the declaring fiber) – lowest priority; nothing for own provisions; stops at an isolation boundary;
3. merge with `service[symbols.mergeIntercept]` (default `Object.assign({}, ...values)` → right-biased).

This is exactly `σ(k)(d(k) ⊕ ι(k))` from Definition 27 with ⊕ = shallow object merge (customizable per service). The `logger` service uses it for `{ name, level }` (`logger.ts:437-448`).

## 5. Design observations

* **Everything is in the root.** There is one `store`, one `props`, one `registry` per root; derived contexts only differ by prototype-chained `isolate`/`intercept` tables and their `fiber`. This keeps "is key k provided in realm r?" a single map lookup.
* **Provider identity** is the `Impl.uid`, a fresh counter value; this implements the "name, not value" discipline of the committed/target view.
* **Isolation per realm symbol** implements the relaxed calculus "disjoint within a realm" (§4.4).
* **No compile-time checking**: declared vs. accessed keys are only checked at runtime by the proxy (paper §5.1.4 notes this could be static).
