# A Calculus of Dynamic Composition (Paper §4.1–4.2)

> **Layer:** Theory. This document is the reference for the *state machine* the Rust core must implement. For how Cordis realizes it, see [`../cordis/04-fiber-lifecycle.md`](../implementation/04-fiber-lifecycle.md).

## 1. Components and fibers (§4.1)

### Definition 48 – component
```
ℭΓ := (d : 𝔇Γ) × (p : 𝔓Γ) × ℑΓ^{d∪p}
```
* `d` – coeffect **specification** (keys required).
* `p` – coeffect **provision** (`Set(K)`, keys it *may* install; no key outside `p` may be installed by its effect function).
* `e` – a witnessed **effect iterator** (witnessed up to `≃_{d∪p}`), the effects contributed while active plus inverses.

### Definition 49 – fiber
A fiber instantiating `(d, p, e)` is `⟨d, p, e, π, σ, τ, θ⟩`:

| Field | Meaning |
|---|---|
| `d, p, e` | from the component |
| `π` | parent fiber name or `root` |
| `σ` | the fiber's **own** coeffect table (empty until it activates) |
| `τ` | retirement flag (⊥ fresh, ⊤ once the orchestrator retired it) |
| `θ` | lifecycle state |

```
Θ := Inactive | Reloading(i, g, ω) | Active(g, ω) | Unloading(g, ω)
   i – remaining iterator, g – accumulator so far, ω : d → 𝔑 – committed view
```
`installed_n(γ) := θ_n ≠ Inactive`. Settled states are `Inactive` (contributes nothing) and `Active` (contributes its effects). `Reloading`/`Unloading` exist because a real transition takes an interval of time. The **committed view ω maps each declared key to the *name of the fiber that provided it* when the transition committed**.

### Definition 50 – registry
A state carries `F_γ : 𝔑 ⇀ 𝔉Γ` whose parent pointers form a tree rooted at `root`. **Names are atoms** – never computed or inspected, only compared for equality; a new fiber draws a name not in use (generative names).

The coeffect context is **derived, not stored**:
```
σ_γ := ⋃ { σ_m | m ∈ dom(F_γ), θ_m = Active(−,−) }      (46)
```
Well-defined because `dom(σ_n) ⊆ p_n` and provisions of distinct fibers are disjoint (O-Insert enforces). Each key thus has at most one possible provider, `provider_k(γ)`.

Important consequences:
* Only **Active** fibers provide. A `Reloading` fiber has written bindings into its own table, but they are not yet visible to dependents; an `Unloading` fiber has stopped providing even though its bindings are still in place. This is what allows a provider to *cease providing before it withdraws anything*.
* A calculus with realms would relax disjointness to "disjoint within a realm" (§4.4). The single shared realm restricts components with non-empty provision to one fiber at a time.

### Definition 51 – registry projection
`σ^S_γ := ⋃ { σ_m|_S | m ∈ dom(F_γ) }` – bindings at keys `S` over **every** fiber (not only Active). Witness conditions are read here, because a transition's own writes stay in the fiber's table before it is Active.

## 2. The nine rules (§4.2)

Two relations: **orchestration** rules `γ ⇒ δ` (actions the orchestrator *may* perform; premises say when legal, not when they occur) and **lifecycle** rules `γ ⟶ δ` (the system takes them unprompted whenever premises hold). Any interleaving is a valid run.

```
            ┌──────── L-Iter ────────┐
            ▼                        │
  O-Insert ──► Inactive ── L-Begin ─► Reloading ── L-Finish ─► Active
  O-Remove ◄──    ▲                      │                        │
                  │                   L-Divert                 L-Leave
                  └────── L-Unload ── Unloading ◄─────────────────┘
  (O-Retire sets τ at any state; omitted from the figure)
```

### 2.1 Orchestration (§4.2.1)
```
O-Insert  n ∉ dom(F)   π ∈ dom(F) ∪ {root}   (d,p,e) ∈ ℭ   ∀m. p ∩ p_m = ∅
          ─────────────────────────────────────────────────────────────
          γ ⇒ γ[n ↦ ⟨d, p, e, π, ∅, ⊥, Inactive⟩]

O-Retire  n ∈ dom(F)                ⟹  γ ⇒ γ[τ_n ↦ ⊤]

O-Remove  τ_n = ⊤   θ_n = Inactive   σ_n = ∅   ∀m. π_m ≠ n
          ⟹  γ ⇒ γ ∖ n
```
* Retiring is a **request**, unconditional; the lifecycle rules carry it out. Retirement is separated from removal so an Active retired fiber is deactivated first (removing it earlier would discard the accumulator and leak).
* O-Remove removes children before parents and only empty-table entries.
* The last premise of O-Insert is the **single-source discipline**.

#### Definition 52 – instantiation
An iteration of `e_n` may instantiate a component: it takes the O-Insert with `π = n` and **yields as its inverse the O-Retire** of the new fiber. The inverse *retires rather than removes* because an inverse must always be applicable, and O-Remove has premises (an Active child would block it). A retired, Inactive, empty entry is *vestigial* (Lemma 62) – indistinguishable from absence. Retiring a child does not make the parent wait; grandchildren are reached one level at a time.

### 2.2 Lifecycle (§4.2.2)

#### Definition 53 – target view and quiescence
```
target_n(γ) := ⊥                                   if τ_n ∨ ¬(γ ⊧ d_n)
               (k ∈ d_n) ↦ provider_k(γ)            otherwise

quiet(γ) := ∀n. (θ_n = Inactive ⇒ target_n = ⊥)
              ∧ (θ_n = Active(−, ω_n) ⇒ target_n = ω_n)
              ∧ no fiber in Reloading/Unloading
```
The target depends only on retirement and coeffect resolution. **Recording a provider name rather than a value** is what makes the comparison usable: a different fiber providing an equal value would otherwise compare equal. (Implementation: `fiber.committed` holds the map, `fiber.target` a hash.)

#### Activation
```
L-Begin   θ_n = Inactive    ω = target_n(γ) ≠ ⊥
          ⟹ γ ⟶ γ[θ_n ↦ Reloading(e_n, id, ω)]

L-Iter    θ_n = Reloading(i, g, ω)   target_n(γ) = ω   i(γ) = (δ, h, Just(i′))
          ⟹ γ ⟶ δ[θ_n ↦ Reloading(i′, g ∘ h, ω)]

L-Finish  θ_n = Reloading(i, g, ω)   target_n(γ) = ω   i(γ) = (δ, h, Nothing)
          ⟹ γ ⟶ δ[θ_n ↦ Active(g ∘ h, ω)]
```
Each iteration composes its inverse as `g ∘ h`, so the accumulator applies inverses LIFO.

#### Deactivation
A deactivation cannot be one step: a consumer torn down *because its provider is leaving* runs teardown code that may need that very provider (e.g. returning connections to a pool). So the **decision** is separated from the **act**, and the act is guarded.

**Definition 54 – relied upon:**
`relied_n(γ) := ∃m ≠ n, k ∈ d_m. installed_m(γ) ∧ ω_m(k) = n`

```
L-Divert  θ_n = Reloading(i, g, ω)   target_n(γ) ≠ ω
          (δ, h) = (γ, id)  ∨  i(γ) = (δ, h, −)          ← abort  or  land the iteration
          ⟹ γ ⟶ δ[θ_n ↦ Unloading(g ∘ h, ω)]

L-Leave   θ_n = Active(g, ω)   target_n(γ) ≠ ω
          ⟹ γ ⟶ γ[θ_n ↦ Unloading(g, ω)]

L-Unload  θ_n = Unloading(g, ω)   ¬relied_n(γ)   g(γ) = δ
          ⟹ γ ⟶ δ[θ_n ↦ Inactive]
```
* **L-Divert** may fall between any two iterations; it routes into Unloading with the inverses accumulated so far (not via Active, which would let dependents activate against a leaving component). The *abort* alternative needs an iteration boundary; the *landing* alternative lets the in-flight iteration complete (needed for asynchronous hosts, §4.4).
* **L-Leave** records the decision only: the fiber stops providing (its table leaves σ_γ) while its own committed view and everybody else's stay intact.
* **L-Unload** is the *only* rule that applies an accumulator; it discards the committed view last. Its premise `¬relied_n` is **the guard**: a provider's withdrawal waits until every consumer that resolved a key to it has gone.
* The guard is **per binding** (tests committed views), and is ordered along coeffects, *not* along the fiber tree: a parent may run its inverse while its child is still Unloading.
* Why the guard does not deadlock: once a provider is marked (L-Leave/L-Divert) its table leaves σ_γ, so no target view can name it any longer, and every consumer committed to it is itself on its way out (Theorem 73).
* The rules are nondeterministic and mention no scheduler – theorems over all sequences hold for every scheduling policy.

### 2.3 Confinement (§4.2.3)

**Definition 55 – confined to n.** A map is confined to `n` if it (1) *writes* only `σ_n` and the values other tables hold at keys of `d_n` (a consumer operating on a coeffect writes the provider's table at that key), never control fields or the registry domain; and (2) *reads* only `σ_n` and `σ_m|_{d_n}`. Instantiation is the one exception (writes a fresh entry; its inverse writes one τ).

**Definition 56 – context-mediated iterators for n.** Least set containing the unit and three iteration forms: lift of an operation stage at a key of `d_n ∪ p_n`; lift of a provision stage at a key of `p_n` (landing in `σ_n`); an instantiation. **Every fiber's effect function must lie in this set.**

**Lemma 57** – members are confined to `n` and witnessed at `≃_{d_n ∪ p_n}`.

Practical meaning: a component may not branch on the lifecycle state of a fiber it did not declare, nor read/write tables outside its two declarations, nor anything no table holds.

## 3. Table 1 – rules as writes

| rule | θ before | θ after | state map Ψ | control fields edited |
|---|---|---|---|---|
| O-Insert | undefined | Inactive | id | dom(F) |
| O-Retire | any | unchanged | id | τ_n |
| O-Remove | Inactive | undefined | id | dom(F) |
| L-Begin | Inactive | Reloading(e_n, id, ω) | id | θ_n |
| L-Iter | Reloading(i,g,ω) | Reloading(i′, g∘h, ω) | pr₁∘i | θ_n |
| L-Finish | Reloading(i,g,ω) | Active(g∘h, ω) | pr₁∘i | θ_n |
| L-Divert | Reloading(i,g,ω) | Unloading(g∘h, ω) | id or pr₁∘i | θ_n |
| L-Leave | Active(g,ω) | Unloading(g, ω) | id | θ_n |
| L-Unload | Unloading(g,ω) | Inactive | g | θ_n |

Every step factors as `γ^{t+1} = edit^t(Ψ^t(γ^t))`: Ψ moves tables (data), edit moves control fields. This seam is what the metatheory exploits.

## 4. Engineering take-aways for an implementation

1. Keep *two* views per fiber: the **target** (recomputed on every relevant change) and the **committed** view (fixed for the whole episode). Transitions fire on their inequality.
2. Identify providers by a **never-reused id** so that "replaced by an equal value" still triggers a reload.
3. A provider counts as available **only while Active**.
4. Mark a deactivating fiber **out of service first** (L-Leave), notify dependents, and run the accumulator only once no installed dependent still commits to it (the guard).
5. Child instantiation is an ordinary tracked effect of the parent; its inverse only *retires* the child.
6. Removal of the registry entry happens only after the fiber is Inactive and empty.
