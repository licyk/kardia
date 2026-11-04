# Revertible Effects (Paper §3.1)

> **Layer:** Theory. Definitions/theorem numbers refer to the paper. For how Cordis implements this see [`../cordis/03-effects.md`](../implementation/03-effects.md); for the Rust mapping see [`../rust-implementation-plan.md`](../rust-implementation-plan.md) §5.

Temporal composability requires that every modification a component makes be **trackable** and **recoverable**. The paper models an effect as a function `Γ → Γ × (Γ → Γ)`: given the current context it returns the modified context *and* an explicit inverse. Returning the inverse to the runtime makes the effect trackable; supplying it makes the effect revertible.

## 1. From impure functions to context transformations (§3.1.1)

Any impure `f : X ⇝ Y` is rewritten into a pure `f : Γ × X → Γ × Y` over a context type Γ. For a fixed input `x`, the side effect is the endomorphism `γ ↦ pr₁(f(γ, x)) : Γ → Γ`. Effects therefore live in the **monoid of transformations** `(Γ → Γ, ∘, id_Γ)`:

* closure – sequential composition of effects is an effect;
* associativity – bracketing does not matter;
* identity – `id_Γ` is the unit.

An undo for `f` is a **left inverse** `g` (`g ∘ f = id`, never required of `f ∘ g`); the paper simply calls it *the inverse*.

### Definition 1 – twisted composition
`(f₁, g₁) ∘ (f₂, g₂) := (f₁ ∘ f₂, g₂ ∘ g₁)` – forwards compose normally, inverses in the opposite order. This makes `(Γ→Γ) × (Γ→Γ)` a monoid **𝔗Γ** (the product of the transformation monoid with its opposite), unit `(id, id)`.

### Definition 2 – effect context
`∂Γ := Γ × (Γ → Γ)`, a pair `(γ, φ)`:
* `γ` – the current state;
* `φ` – the **accumulator**, the composite of inverses so far; it recovers the initial state.
The initial effect context is `(γ₀, id)`. Iterating gives the tower `Γ, ∂Γ, ∂²Γ, …`.

### Definition 3 – track
`track_Γ(f, g) := (γ, φ) ↦ (f(γ), φ ∘ g)`.

* **Theorem 4** – tracking leaves the forward behaviour untouched: `pr₁ ∘ track(f,g) = f ∘ pr₁`.
* **Theorem 5** – `track` is a monoid homomorphism `𝔗Γ → (∂Γ → ∂Γ)`: tracking pairs one at a time equals tracking their twisted composite.

### Definition 6 – recover
`recover_Γ(γ, φ) := (φ(γ), id)` – apply the accumulator, reset it.

* **Theorem 7** – for `g(f(γ)) = γ`: `recover(track(f,g)(γ,φ)) = recover(γ,φ)`. A tracked step whose inverse reverts it does not move the recovery target. The **soundness invariant** of a state is `φ(γ) = γ₀`; every state reached from `(γ₀, id)` by such steps satisfies it, and `recover` carries it back to `(γ₀, id)`.
* By Theorem 5 the same holds for whole sequences (eq. 11).

## 2. Effect functions (§3.1.2)

Two limitations of `track/recover`:
1. `track(f, g)` fixes `g` before the state is seen – a single uniform inverse must work at every state. Real effects only need an inverse *for the state where they were applied* (e.g. "remove the entry I just added, with the id I just drew").
2. `recover` is all-or-nothing – you cannot undo one effect and keep others.

Fixes: return the inverse at the point of application (input side, `Γ → ∂Γ`), and make the lifted effect itself return an inverse (output side, `∂Γ → ∂²Γ`). Both have the same shape.

### Definition 8 – effect function and witnessed effect function
* `𝔈Γ := Γ → Γ × (Γ → Γ)`, `e(γ) = (δ, g)`: new context `δ`, inverse `g`.
* `𝔈Γ*` additionally carries a **witness** `(δ, g) = e(γ) ⇒ g(δ) = γ`: the inverse reverts the effect *at the state where it was applied*. A uniform inverse with `g ∘ f = id` is a special case.

### Definition 9 – effect composition ⋄
`(f ⋄ g)(γ) := let (δ, s) = g(γ); (ε, t) = f(δ) in (ε, s ∘ t)` – run `g` then `f`, compose inverses so that `f`'s inverse runs first.

* **Theorem 10** – `(𝔈Γ, ⋄)` is a monoid with unit `η(γ) = (γ, id)`; `(f,g) ↦ γ ↦ (f(γ), g)` is a homomorphism `𝔗Γ → 𝔈Γ`.
* **Theorem 11** – `𝔈Γ*` is a submonoid (witnessing survives composition) and uniform inverses land in it.

### Definition 12 – effect (lifting to the effect context)
```
effect_Γ : 𝔈Γ → ∂Γ → ∂²Γ
effect_Γ(e)(γ, φ) = let (δ, g) = e(γ) in ((δ, φ ∘ g), track_Γ(g, pr₁ ∘ e))
```
The returned inverse is itself a *track* of the swapped pair: undoing is an effect (transform by `g`), and undoing the undo is performing the effect again (`pr₁ ∘ e`).

* **Theorem 13** – `effect(f) ⋄ effect(g) = effect(f ⋄ g)`.
* **Theorem 14** – projections commute: the lifted forward and inverse maps act on the state exactly as `f` and `g`.
* **Theorem 15** – for witnessed `e`: `g′(Δ) = (γ, φ ∘ g ∘ f)`. The *state* is recovered exactly; the *accumulator* is restored iff `g ∘ f = id`; in every case `(φ ∘ g ∘ f)(γ) = φ(γ)`, so the soundness invariant is preserved. Hence `effect` does **not** carry 𝔈Γ* into 𝔈*∂Γ in general, but reverting never moves the recovery target.

## 3. Effect iterators (§3.1.3)

A component's loading is not one effect but a **sequence**.

* **Theorem 16** – applying `e₁…eₙ ∈ 𝔈Γ*` from `(γ₀, id)` and reverting in **reverse order**: every revert recovers the state its application ran against, and every intermediate state satisfies the soundness invariant. LIFO needs nothing more.

### Definition 17 – effect iterator
```
ℑΓ  := μℑ. Γ → Γ × (Γ→Γ) × Maybe(ℑ)
ℑΓ* := the witnessed version (each iteration's inverse reverts at its state;
       the continuation is again witnessed)
```
Each iteration yields `(δ, g, o)`: new context, inverse, continuation (`Nothing` = done, `Just(i)` = next).

### Definition 18 – effect iterator transformation
```
effect^iter_Γ(i)(γ, φ) =
  let (δ, g, o) = i(γ); t = track_Γ(g, pr₁ ∘ i) in
  match o
  | Nothing  ⇒ ((δ, φ ∘ g), t)
  | Just(i′) ⇒ let (s, r) = effect^iter_Γ(i′)(δ, φ ∘ g) in (s, t ∘ r)
```
* The accumulator becomes `φ ∘ g₁ ∘ … ∘ g_k`, which reverts in LIFO order (Theorem 16).
* `effect^iter` lands in the same `∂Γ → ∂²Γ` as `effect`, so **an iterator is an effect** and composes anywhere an effect can (nesting).
* The `Maybe(ℑ)` continuation makes a **boundary** available between any two iterations: at a boundary the accumulator recovers exactly the iterations so far. This is the hook used later for *interruption* (L-Divert) and *cancellation*.
* The iterator is a **reified delimited continuation** – mainstream languages expose it via `yield`, so the model "maps directly onto the generators they already provide".
* A plain effect function embeds as an iterator whose first iteration yields `Nothing` (eq. 19).

## 4. What local temporal composability means

> For every sequence of effect functions a component applies, the accumulator recovers the context it began at (Theorem 7), and reverting the sequence hands each inverse the state its own application ran against (Theorem 16). Loading a component is running one iterator and accumulating its inverses in φ; unloading it is applying φ.

Two things are deliberately **left out** and supplied later by *independence* (§3.4): reverting out of the order the accumulator imposes, and sequences that interleave the effects of other components.

## 5. Engineering take-aways (for implementers)

| Formal notion | Practical reading |
|---|---|
| Effect function returns inverse | Every registration API returns a disposer created *at the point of registration* (it can capture the id it just drew). |
| Witness | An obligation on the author: the disposer must undo exactly that registration. Not checked at runtime. |
| ⋄ / twisted composition | Disposers of a composite effect run in reverse order of their creation. |
| `effect` returns an inverse at the next level | `ctx.effect(...)` itself returns a disposer, and that disposer is registered in the *enclosing* scope's accumulator – effects nest. |
| Iterator boundary | A multi-step setup can be cancelled *between* steps, keeping only the inverses of completed steps. |
| Soundness invariant | Disposing twice is forbidden – an inverse applied at a state no application produced reverts nothing meaningful (the implementation enforces "at most once"). |
