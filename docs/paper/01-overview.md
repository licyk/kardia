# Paper Overview: *A Programming Paradigm for Spatiotemporal Composability*

> **Layer:** Theory (the paper, [arXiv:2608.25512](https://arxiv.org/abs/2608.25512), 92 pages, Shi / Zhang / Cui, Peking University & DeepSeek-AI).
> This document summarizes the paper's problem statement, contributions and vocabulary. It does not describe the Cordis code base (see `docs/cordis/`) or the Rust plan (see `docs/rust-implementation-plan.md`).

## 1. Problem statement

Static composition (function calls, imports, inheritance) is fixed at compile time. Plugin systems and *self-evolving agent harnesses* need **dynamic composition**: components are loaded, unloaded and reconfigured while the process keeps running. The paper argues that this has two orthogonal dimensions beyond the algebraic aspects of composition (§1.1):

| Dimension | Requirement | Static analogue |
|---|---|---|
| **Temporal composability** | When a component is removed, every modification it made to the shared environment is completely and safely reversed (resources, event registrations, state mutations), in an orderly way. | Lexical scoping: RAII, bracket patterns |
| **Spatial composability** | Components declare, discover and resolve dependencies on one another in a structured, verifiable way; lifecycles are coordinated as dependencies appear, disappear or change identity. | Module import resolution |

### Motivating examples (§1.2)

* **VSCode** – extensions cannot be unloaded individually (87 of the top 100 contain code and require an extension-host restart); `deactivate` is only a shutdown hook and separates disposal from creation. `extensionDependencies` is barely used (7 of top 100) and inter-extension APIs are untyped (`exports: any`).
* **Agent harnesses** – a harness that rewrites its own components continuously cannot afford a restart per modification, and naive code replacement silently breaks dependents.
* **Coarse-grained workaround** – OS processes give temporal composability per process; container orchestrators give spatial composability per service. Both discard in-process state on restart and cannot express in-address-space dependencies; the granularity is wrong.

## 2. Core idea

Effect systems describe how a computation **modifies** its environment; coeffect systems describe what it **requires** from its environment. Both are classically *static* (checked over lexically fixed scopes). The paper **reifies** both as runtime objects:

1. **Revertible effects** (§3.1): every context transformation is paired with an inverse that the runtime holds. Tracking and recovery are monoid homomorphisms, so the context is recovered upon removal → *local temporal composability*.
2. **Reactive coeffects** (§3.2): a component declares the keys it requires (a *specification*); every context change is classified against it as *activating*, *deactivating* or *neutral*, which drives activation/deactivation → *local spatial composability*.
3. **The context paradigm** (§3.3): effect context and coeffect context are unified into one context type Γ∞, and *every* effect and coeffect is mediated through it. This induces an **observational equivalence** ≃ under which the effects of distinct components become **independent** (§3.4).
4. **A calculus of dynamic composition** (§4): a component is a triple *(d, p, e)* (dependencies, provisions, effect iterator); its instantiations are **fibers**; nine rules (3 orchestration, 6 lifecycle) move fibers through `Inactive → Reloading → Active → Unloading`. The metatheory proves preservation, recovery exactness, ordering, resolution coherence, progress (no deadlock + termination) and **confluence** (the quiescent state equals a from-scratch static load of the final configuration).
5. **Cordis** (§5): a TypeScript meta-framework – a core library (effect tracking + coeffect resolution) and a declarative loader with configuration reconciliation and hot module replacement. Case study: **Koishi**, a chatbot framework with >4000 community plugins (Koishi uses Cordis v3; the paper describes v4).

## 3. Paper structure and where it is summarized

| Paper section | Topic | Summary document |
|---|---|---|
| §1–2 | Motivation, effects & coeffects preliminaries | this file |
| §3.1 | Revertible effects (effect context, track/recover, effect functions, effect iterators) | [`02-revertible-effects.md`](02-revertible-effects.md) |
| §3.2 | Reactive coeffects (coeffect context, specification & notification, isolation, interception) | [`03-reactive-coeffects.md`](03-reactive-coeffects.md) |
| §3.3–3.4 | Unified context, coeffect operations, observational equivalence, independence, commutativity | [`04-context-paradigm.md`](04-context-paradigm.md) |
| §4.1–4.2 | Components, fibers, registry, the nine rules, guard, confinement | [`05-calculus.md`](05-calculus.md) |
| §4.3–4.4 | Metatheory, extensions (asynchrony, failure, isolation, configuration) | [`06-metatheory.md`](06-metatheory.md) |
| §5 | Implementation as described by the paper (Algorithms 1–10), loader, HMR, Koishi | [`07-implementation-in-paper.md`](07-implementation-in-paper.md) |
| §6–8 | Discussion, related work, conclusion | [`08-discussion-and-related-work.md`](08-discussion-and-related-work.md) |

## 4. Preliminaries (§2) in brief

* **Effects.** Judgments `Γ ⊢ t : T ! effect`. Monadic effects (Moggi, Wadler) encapsulate effectful computation in `T(A)` with unit η and multiplication μ. Algebraic effects (Plotkin & Power) declare operation signatures; effect handlers (Plotkin & Pretnar) interpret them with delimited continuations (Koka, Eff, OCaml 5).
* **Coeffects.** Judgments `Γ @ coeffect ⊢ t : T` annotate the *context*. Comonads `(D, ε, δ)` (Uustalu & Vene) structure context-dependent computation; Petricek et al. propose coeffects as a unified static analysis; graded coeffects use a pre-ordered semiring to quantify usage.
* **Gap.** Both are static instruments over lexically fixed scopes. No fixed lexical scope delimits a plugin loaded after deployment, and no compile-time context anticipates dependencies arising from runtime configuration. The paper therefore *reifies* the structures so a runtime can operate on them and establish dynamically what these systems guarantee statically.

## 5. Glossary (paper vocabulary)

| Term | Meaning |
|---|---|
| Context type Γ | Runtime-operable reification of the typing context; `Γ∞ = μΓ. Γ × (Γ→Γ) × Σ`. |
| Effect context ∂Γ | `Γ × (Γ→Γ)`: a state plus an *accumulator* (composite of inverses). |
| Accumulator φ | Function that recovers the context to its initial state; LIFO composite of inverses. |
| Effect function 𝔈Γ | `Γ → Γ × (Γ→Γ)`: returns new state and a per-state inverse. Witnessed (𝔈Γ*) when the inverse provably reverts at the applied state. |
| Effect iterator ℑΓ | `μℑ. Γ → Γ × (Γ→Γ) × Maybe(ℑ)`: a sequence of effects with a boundary between consecutive iterations; a reified delimited continuation (generators). |
| Coeffect context Σ | Finite dependent partial map `(k:K) ⇀ V_k`. |
| Specification d | `Set(K)`: keys a component requires. |
| Provision p | `Set(K)`: keys a component may install. |
| notify_d(σ, σ′) | Classification of a transition: activating / deactivating / neutral. |
| Isolation realm | `ρ : K ⇀ R`; key → realm → value (two-layer resolution). |
| Interception metadata | Per-key monoid `(M_k, ⊕_k, ε_k)`; merged component-declared ⊕ context-carried (right-biased). |
| Coeffect operations 𝒜_k | Operations a value at key `k` offers; induce the observational equivalence ≃_k. |
| Commutative key | All operations at the key pairwise independent; a *witnessed coeffect* carries that proof. |
| Component ℭΓ | Triple `(d, p, e)`. |
| Fiber | Instantiation `⟨d, p, e, π, σ, τ, θ⟩` (parent, own table, retirement flag, lifecycle state). |
| Lifecycle state θ | `Inactive | Reloading(i,g,ω) | Active(g,ω) | Unloading(g,ω)`. |
| Committed view ω | Map from declared keys to the *names of providing fibers* at commit time. |
| Target view | What ω should be now (`⊥` if retired or unsatisfied). |
| Guard (relied_n) | L-Unload may fire only when no installed fiber's committed view names `n`. |
| Quiescent | Every fiber settled at its target. |
| Support set A | Fibers that are not retired, whose parent is supported, and whose declared keys are provided by supported fibers. |
| Inertia | Asynchronous hosts cannot abort an iteration in flight; only the *landing* alternative of L-Divert is available. |
| System boundary | Locations the system can exclusively modify and restore lie inside; others lie outside (emissions cross it). |
