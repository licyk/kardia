# Cordis Core: Services, Traceable Proxies and Caller Attribution

> **Layer:** Implementation (`cordis/packages/core/src/service.ts`, `utils.ts:359-479`).
> Paper counterpart: §5.1.4 (context access) and §6.3 (capabilities). This mechanism is mostly *implicit* in the paper but is essential for temporal composability in practice.

## 1. The problem it solves

A service is provided by one fiber (the provider) and called by many others (consumers). When a consumer calls, say, `ctx.timer.setTimeout(cb, 100)`, the timer it creates is an effect that must be **owned by the consumer's fiber** – otherwise unloading the consumer would leak the timer, and unloading the provider would kill timers it does not conceptually own. Likewise, `ctx.logger('x')` should be named after the calling plugin. Cordis therefore distinguishes two contexts on every service call:

* **use site** – the context the service was accessed from (the caller). Governs *effects*, *interception*, *isolation*.
* **def site** – the context where the accessing code was defined (the service's own context when code inside the service runs). Governs *service resolution* (which `inject` declarations admit access).

## 2. `Service` base class (`service.ts`)

```ts
abstract class Service<T = never> {
  static init / check / invoke / metadata / extend / tracker / mergeIntercept / resolveIntercept  // well-known symbols
  name: string
  constructor(protected ctx: Context, name: string) {
    // callable services: if this[symbols.invoke] exists, `self` becomes a function object
    self.ctx = ctx; self.name = name
    self[symbols.tracker] = { associate: name, property: 'ctx' }
    ctx.reflect.provide(name, self, this[symbols.check])    // provision (a tracked effect)
    return self
  }
}
```
* Constructing a `Service` inside a plugin's `apply` provides it – the provision is a revertible effect of that fiber.
* `[Service.init]()` is called after construction by the fiber runner (`fiber.ts:158-165`); its return value is treated as an effect (may be async/generator).
* `[Service.check]()` – an extra availability predicate consulted by consumers' `_checkImpl`.
* `[Service.invoke]` – makes the service callable (`ctx.logger('name')`).
* `[Service.filter](ctx)` – default realm-equality filter for events emitted by the service.
* `[Service.extend](props)` – create a derived instance (prototype child or callable clone).
* `[Service.mergeIntercept]` / `[Service.resolveIntercept]` – interception metadata (see [02](02-context-and-reflection.md#4-interception)).
* `Service[Symbol.hasInstance]` walks constructors because instances may be proxies.

## 3. The tracker and traceable proxies (`utils.ts`)

Any object with a `[symbols.tracker]` descriptor `{ property?: 'ctx', associate?: string, noShadow?: boolean }` is wrapped by `getTraceable(ctx, value)` whenever it is read off a context. `createTraceable` (`utils.ts:410-465`) returns a Proxy that:

| Access | Result |
|---|---|
| `value[tracker.property]` (i.e. `service.ctx`) | the **use-site** context |
| `value[symbols.caller]` | the def site (the caller's def site) |
| `value[symbols.original]` | the raw object |
| `value.foo` where `"${associate}.foo"` is a declared context property | the associated property (e.g. service `foo.bar` reachable as `ctx.foo.bar`; mixins into associated types such as sessions) |
| a getter / method | invoked with a **shadow receiver** whose `ctx` is `useSite.extend({ [symbols.shadow]: defSite })` |
| a nested traceable value | traced again with the use site |
| calling a callable service | `[symbols.invoke]` applied to the shadow receiver |

The **shadow** context (`symbols.shadow`) is a pair: the object is the use site (its fiber receives effects), its `shadow` property is the def site (consulted by the proxy `get` trap for `locate`). Hence inside a service method:
* `this.ctx.effect(...)`, `this.ctx.on(...)` → registered in the **caller's** fiber;
* `this.ctx.otherService` → resolved against the **service's** own `inject` (def site), even when called from root (`shadow.spec.ts: 'applies the service inject when entered from root'`);
* `noShadow` services (events, logger, reflect, registry) expose the caller without preserving shadows.

`withProps` / `withProp` build lightweight proxies overriding a few properties; `joinPrototype` merges a class prototype with `Function.prototype` for callable services; `createCallable` builds the function object.

## 4. Associated properties (`tests/associate.spec.ts`)

A service named `foo.bar` (or a property provided as `foo.bar`) is reachable as `ctx.foo.bar`. Objects created by a service (e.g. a `Session` with tracker `associate: 'session'`) expose mixins registered under `session.*` by *other* plugins (`ctx.mixin('bar', { answer: 'session.answer' })`). Visibility follows the declaring fiber (`'associated access follows the service fiber'`). This is how independent plugins extend each other's types at runtime – the dynamic counterpart of TypeScript module augmentation (paper §6.4).

## 5. Why it matters for a port

| Concern | JS mechanism | Must be preserved as |
|---|---|---|
| Effects attributed to the caller | `service.ctx` returns use-site ctx via Proxy | service methods must receive/obtain the caller's context explicitly |
| Service resolution from def site | shadow context in `locate` | services resolve their *own* dependencies from their own fiber's committed view |
| Callable services | Function objects with prototype join | ordinary methods (`logger.named("x")`) |
| Associated extension | `associate` + mixin accessors | extension traits / registries keyed by `(type, name)` |
| Capability checking | Proxy `get` trap | typed handles obtainable only via declared injection |

Cordis's traceability is ~200 lines of Proxy plumbing that has no direct Rust equivalent; the Rust plan replaces it with an explicit `Handle<S>` = (service `Arc`, caller `Context`) – see the plan §6.
