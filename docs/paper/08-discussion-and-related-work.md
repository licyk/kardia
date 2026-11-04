# Discussion, Related Work and Conclusion (Paper §6–8)

> **Layer:** Theory / design guidance. Sections relevant to a Rust port are flagged with **[Rust]**.

## 1. System boundary (§6.1)

The boundary divides the environment into locations **inside** (the system can modify exclusively *and* restore → tracked, revertible) and **outside** (either ability fails → the operation acts as `id_Γ`, untracked).

* **Coeffects move the boundary** by reifying an external location behind operations that each have an inverse. The boundary is drawn *per location*, not per medium (a private scratch file is inside; a shared path is outside).
* **Acquisition vs. emission.** Reaching outside happens in two stages: *acquisition* installs a record inside the boundary (`open`→descriptor, `malloc`→block, `fork`→child), which is revertible; *emission* pushes data through that channel (`write`, `send`) and crosses outside – not revertible.
* **Withholding and compensation.** Recovering an emission requires either withholding it until the producing state is certain (output-commit problem) or compensation (delete the created file, refund the charge) up to an application-supplied, coarser equivalence. Compensations compose LIFO like inverses, but the commutation metatheory must be re-established against the coarser relation.

## 2. Service multiplexing (§6.2)

Coeffects echo OSGi services: providers publish under an interface, consumers bind. Multiple implementations can be realized as:
1. **Exclusive binding** – at most one bound at a time; switching unloads one provider and loads another, perturbing every consumer.
2. **Service broker** – a central service is injected by both backing providers and consumers; providers *register with the broker* via revertible effects; consumers see no change when a backing provider changes.

The broker enables **load balancing** (policy-driven dispatch; scale by loading/unloading providers), **rolling updates** (load new provider, shift traffic, unload old once drained – quiescence/tranquility), and **cross-process invocation** (each process hosts its own context; RPC preserves the interface; must be designed against an *asynchronous* contract).

**[Rust]** The broker pattern is the recommended way to get many providers per key within the single-provider calculus.

## 3. Access control and sandboxing (§6.3)

* **Capability-based access control.** Proxy-mediated access only allows declared dependencies (undeclared access errors). `inject` acts as a capability request, the context proxy as a capability mediator. Because declarations are static, the orchestrator can review them at load time.
* **Fine-grained policy via interception** – e.g. a filesystem dependency carrying metadata about readable/writable paths, checked by the provider; adjustable from the context without modifying the provider; affects how a dependency is used, not whether it is satisfied, so it can change at runtime without reload.
* **Sandboxing** untrusted code needs an execution boundary beyond the language (SFI, separate runtime, sandboxed process, container); the untrusted component reaches host dependencies through a *bridge*, itself an ordinary fiber whose capabilities can be attenuated.

## 4. Language independence and selection (§6.4) **[Rust]**

**Temporal requirements:** closures (an inverse is a captured value), plus a way to introduce and retract a component's code at runtime:
* managed runtimes: programmatic module registry (Node `require.cache`; ESM has no public eviction API);
* **native code: explicit dynamic linking/unlinking** (`dlopen/dlclose`, `LoadLibrary/FreeLibrary`);
* **WebAssembly**: an instance is released when a native embedder (e.g. **Wasmtime**) drops it.
In all cases loading is an effect whose inverses undo the registrations the module introduced.

**Spatial requirements:** a DI mechanism at two levels:
* **type level** – a well-typed dependency access; the context type must record each key's coeffect. *Typeclasses (Haskell) and **traits (Rust)** let a provider extend the context type from its own module through an instance/`impl`*; TypeScript uses module augmentation.
* **runtime level** – access must be dynamically mediated (JS `Proxy`, Python descriptors, or reflection at the cost of type safety).
* **Metaprogramming** supplies both together: *compile-time metaprogramming (e.g. **Rust procedural macros**, Scala macros, Zig comptime) emits, for each dependency, a typed declaration together with an accessor, dispensing with a general-purpose interception primitive.*

## 5. Mutual dependencies and component granularity (§6.5)

A dependency cycle leaves the involved components **permanently inactive** – unlike a concurrency deadlock it is *predictable from the declarations*, so a runtime can report it at load time. Most cycles decompose: e.g. server ↔ access-controller becomes `server-core`, `access-control-core`, `request-mediation` (depends on both cores) and `policy-management` (depends on both). Cost: potentially quadratic number of integration components – mitigated by package bundling, convention-based wiring, scaffolding.

## 6. Dependency typing and versioning (§6.6)

Linking is purely **nominal** (by key). Two problems across independently built components:
* **interface drift** – the provider changes the interface behind `k` between versions;
* **key collision** – unrelated providers choose the same key name.

Approaches: (1) **key namespacing** `K × P` (by package; eliminates collisions, couples to a registry); (2) **peer dependencies** with semver ranges via the package manager (Cordis's current approach; relies on semver discipline and one version per package); (3) **structural compatibility** predicates (width subtyping is easy; behavioural contracts hard; bounded quantification undecidable). Open problem.

**[Rust]** Rust's `TypeId`/type-keyed services give collision-freedom *within one binary* (a form of key namespacing), but across dynamically loaded modules `TypeId` is not stable – keys must then be strings plus an ABI/version tag.

## 7. Co-design with languages and operating systems (§6.7)

* **Languages:** make the context implicit again (ergonomics, and prevents a component from reaching another component's context through a closure/global, which would leak effects out of its lifecycle); give the compiler syntax for effects (compile an effect iterator into one state machine holding inverses in its frame instead of a closure per step); admit the coeffect specification into the type system (report cycles at compile time; structural dependency comparison à la row types).
* **Operating systems:** make the declared coeffect specification the whole of what a component can reach (like WASM imports), provide isolation/interception natively, and provide OS resources (memory, fds) as coeffects so acquisitions are recorded once by the party that hands them out; transactional/copy-on-write storage can make some emissions revertible.

## 8. Related work (§7)

| Area | Representative work | Distinction from Cordis |
|---|---|---|
| Monadic effect systems | ZIO, Effect-TS, fp-ts | Require writing inside the effect type; withdrawn service leaves effects in place. Cordis is an overlay on ordinary code and pairs every effect with an inverse. |
| Algebraic effects as capabilities | Effekt | Purpose (interpretation vs. tracking/reversion) and setting (static vs. runtime). |
| Reversible effect semantics | Heunen et al. (dagger/inverse arrows) | Global two-sided reversibility vs. per-atomic-effect one-sided inverse supplied at application. |
| Graded types | Granule, coeffects for Java-like languages | Static annotations over fixed scopes. |
| Context-oriented programming | COP layers | Changes method dispatch in dynamic extents; no tracking/reversion; activation not dependency-driven. Cordis can express only COP's global, value-driven fragment. |
| Aspect-oriented programming | AspectJ, dynamic AOP | Oblivious quantification vs. declared coeffects; Cordis crosscutting is bound to a component's lifecycle. |
| Stateful forward migration | Quiescence/tranquility, DSU (Ginseng, Kitsune), Erlang `code_change`, Webpack/Vite HMR | Migrate state forward via hand-written functions; Cordis reverts and reapplies from clean slate (in-memory state survives only in longer-lived dependencies; DSU-style migration atop revertible effects is future work), but needs no migration code and supports full unloading. |
| Developer-authored recovery | OSGi/IDE unload callbacks, Command pattern, sagas, effect-handler finalizers, event sourcing, React `useEffect` | Inverse is an unenforced duty decoupled from the operation; `useEffect` pairs but is not composable (top-level only, no async/iterator). |
| Statically scoped reversal | STM, reversible computing (Janus), RCCS, linear types, **RAII, Rust ownership** | Scope and reach fixed statically. Cordis treats lexical resource management as *complementary*, appropriate for local resources within a component. |
| Interposed reclamation | Nooks, shadow drivers, Akeso | Platform fixes what can be recorded; Cordis lets components introduce their own effects with inverses and propagates removal to dependents. |
| Initialization-time DI | Spring, Guice, Angular, Inversify, Vue provide/inject, React Context | No reactive re-resolution, no lifecycle management. |
| Availability-reactive components | OSGi Declarative Services, iPOJO, R-OSGi | Closest precedent; but hand-written, synchronous deactivation callbacks. Cordis reverts automatically and runs asynchronous teardown to completion (inertial Unloading). |
| Value-level reactivity | FRP, signals | Value granularity and glitch freedom per turn; Cordis is component-granular with async lifecycle; complementary (a coeffect may carry signals). |

**[Rust] note on RAII:** the paper explicitly positions Rust's ownership/RAII as *statically scoped* reversal – complementary, not a substitute. A Rust port should therefore *not* try to make a component's lifetime a lexical scope; component lifetime is dynamic and governed by the fiber state machine, while `Drop` remains useful for resources local to one effect.

## 9. Conclusion (§8)

Revertible effects give local temporal composability; reactive coeffects give local spatial composability; the context paradigm and observational equivalence give independence; the calculus lifts both to whole systems with preservation, recovery exactness, ordering, coherence, progress and confluence. Cordis realizes this; Koishi validates it. Future validation target: **self-evolving agent harnesses**, which stress complete recovery under rapid replacement and dependency coordination under frequent topological change.
