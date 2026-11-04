# Metatheory and Extensions (Paper §4.3–4.4)

> **Layer:** Theory. Each theorem below is stated informally with the practical guarantee it buys and the implementation obligation it implies. The Rust plan (§11, testing) turns several of these into property tests.

## 1. Setup (§4.3, Definition 58)

Steps are indexed by `t`; `step^t = r(n)` is the rule applied and the fiber it acts on. Runs start from an empty registry. An **episode** of `n` is a maximal interval during which `installed_n` holds; it opens just after an L-Begin and closes at an L-Unload. Every step factors as `γ^{t+1} = edit^t(Ψ^t(γ^t))`; tables (`σ_m`) move only inside Ψ, control fields (`θ, τ, π, d, p, e`, registry domain) only inside edit (except via instantiation). States are compared with a ≃ that keeps all control fields (eq. 54).

**Lemma 59 (lookups in Table 1):**
1. a table moves only inside a Ψ of its own fiber or of a fiber declaring keys it holds (values only, domain unchanged);
2. `ω_n` is created only by L-Begin and destroyed only by L-Unload → **constant over an episode**;
3. only L-Unload(n) applies `g_n`;
4. episodes open by L-Begin and close by L-Unload;
5. `π, d, p, e` are written once at insertion; `τ` is monotone (only O-Retire writes it, only to ⊤).

**Lemma 60 (≃-invariance)** – rules cannot distinguish ≃-related states. **Lemma 61 (equivariance)** – results hold up to renaming fiber names. **Lemma 62 (vestigial entries)** – a retired, Inactive, empty, childless entry is invisible to all rules.

## 2. Preservation (§4.3.1)

**Definition 63 – well-formed registry:** (1) parents exist (or root); (2) provisions pairwise disjoint; (3) an installed fiber's ω is total on `d_n` and names existing fibers; (4) every fiber an installed ω names is installed.

**Theorem 64** – every rule preserves well-formedness. The guard on L-Unload is what carries clauses (3)/(4): no committed view ever names a removed fiber. Consequences: a name freed by O-Remove may be reissued (no stale views), and a fiber can be removed as soon as it is Inactive without a separate dependency check.

## 3. Temporal composability, global form (§4.3.2)

**Definition 65** – fibers `m, n` are *entangled* when one's provision meets the other's declarations. A run is *pairwise independent* when every pair of fibers is independent, or entangled with only commutative shared keys.

**Lemma 66** – every run is pairwise independent (supplied by witnessed coeffects + Theorem 47).

**Lemma 67 (entangled steps)** – while `n`'s episode is open, a provider `m` of `n` takes only identity state maps (the guard blocks its L-Unload); a consumer's steps commute with `g_n`; while `n` is Reloading, entangled fibers take only identity maps.

**Theorem 68 (Recovery exactness)** – applying `n`'s accumulator at any point of its episode leaves every table where the *other* fibers' steps alone would have left it from the episode start:
`g_n^u(γ^u) ≃_K (Ψ^{t_l} ∘ … ∘ Ψ^{t_1})(γ^b)`.
→ *Running an inverse withdraws the fiber's contribution and nothing else.*

**Corollary 69 (Terminal recovery)** – after an episode closes, the state equals "other fibers only"; in particular `σ_n = ∅` (which is O-Remove's premise).

*Bound:* tables are restored only up to each key's ≃_k – a monotone allocator is not rewound, heap layout is not restored, a sent message stays sent.

## 4. Spatial composability, global form (§4.3.3)

**Theorem 70 (Ordering)**
* A fiber begins a transition only where its dependencies are provided (`L-Begin(m) ⇒ γ ⊧ d_m`).
* If `m` committed key `k` to provider `n`, then throughout `m`'s episode: `ω_m(k) = n`; `n`'s episode strictly contains `m`'s (`b < b′`, and `u′ < u` if `n` closes) → **a provider outlives its consumers**; the binding `k` stays in `σ_n` and moves only by operations at `k` of fibers declaring `k`.

**Theorem 71 (Resolution coherence)** – every iteration of a transition runs against the one committed resolution ω. When the fiber leaves Reloading, either L-Finish lands it Active against ω, or L-Divert takes it out and its episode closes with exact recovery. A transition never straddles two resolutions of its coeffects (no glitch *within* a transition; Cordis has no global "turn" like FRP).

## 5. Progress (§4.3.4)

**Definition 72** – precedence `n ≺ m := p_n ∩ d_m ≠ ∅` (n may provide a key m declares). Acyclicity of ≺ is an **assumption** (a component that declares a key it provides itself violates it).

**Theorem 73 (Progress)** – assume ≺ acyclic, iterator length ≤ K, finitely many names. Then:
1. **No deadlock** – a non-quiescent state always admits some lifecycle rule (the chain of Unloading fibers blocked by relied-upon consumers is ≺-increasing, hence finite, and ends at an unblocked one).
2. **Termination** – each fiber takes at most `(K + 3)(V(n) + 1)` steps, `V(n)` = number of target turns, which is finite.

Every maximal sequence of lifecycle steps ends quiescent. Finiteness of names requires that no component instantiates itself without bound.

## 6. Confluence (§4.3.5)

**Definition 74 – support set A**: `n ∈ A ⇔ ¬τ_n ∧ (π_n = root ∨ π_n ∈ A) ∧ ∀k ∈ d_n. ∃m ∈ A. k ∈ p_m` – a function of `τ, π, d, p` only. **Lemma 75** – the support relation is well-founded and A unique.

**Definition 76 – total on its provision**: a finished activation has installed every key of `p`. **Lemma 77** – at quiescence (with totality), A = the set of Active fibers.

**Lemma 78 (Transposition)** and **Lemma 79 (Deletion)** – adjacent independent steps can be swapped, and a closed episode (with its instantiations) can be deleted without changing the endpoint.

**Theorem 80 (Confluence)** – any run reaching a quiescent state can be reduced to a **canonical** run: same orchestration steps, then one episode per supported fiber in an order linearizing support. Two runs with the same orchestration inputs reach ≃-related states (up to renaming). *The lifecycle relation has unique normal forms.*

> "The theorem is what licenses reasoning about a Cordis application as though it were statically assembled." Adding, removing, replacing and un-replacing components arrives at the state of writing the final composition down at the outset. It speaks about *state*, not about emissions made along the way.

## 7. Extensions (§4.4)

### Asynchrony / inertia
In an async host iterations and inverses are futures that run to completion once started, so the **aborting** alternative of L-Divert is unavailable. Such a host is **inertial**: a fiber whose target turns during an iteration deactivates after that iteration lands. All results quantify over all runs, so they cover inertial runs; Theorem 73 never uses the aborting alternative. Inverses in flight need no counterpart (Unloading is never declined). A deactivation may chain directly back into an activation (implementation: `reload`/`unload` mutual chaining).

### Failure
An iteration may raise: `Γ → Either(Ξ, Γ × (Γ→Γ) × Maybe(ℑ))`. A raise exits Reloading like an aborting L-Divert (chosen by the iteration, not the environment): the fiber unwinds through Unloading with the accumulator built so far, arrives Inactive having installed nothing, and the **error is recorded on the fiber**. L-Begin then requires an error-free fiber – a failed effect is *not retried against an unchanged environment*; `quiet` admits failed fibers; failure does not propagate to the parent (siblings keep running). A retry is a revision (re-insertion without the outcome). Confluence excludes failed fibers. → Implementation state `FAILED`.

### Isolation
With realms fixed at insertion, the relaxed calculus is the present one over keys `K × R`: a fiber under realm table ρ declares/provides `(k, ρ(k))`; disjointness becomes "disjoint within a realm". Reassigning a realm at runtime changes a fiber's interface, so it is handled as a *revision*. Interception needs no extension: metadata is consulted at access time and is in no fiber field.

### Configuration
A component takes a configuration bound into its effect function; one component with two payloads is two components. The declarative layer may **revise** a running fiber (replace config, reassign realms, disable/re-enable). Disabling is O-Retire; every other revision = retire → deactivate → remove (children first) → reinsert at the same name. Re-enabling uses this composite rather than resetting τ to ⊥ (τ monotonicity underlies Theorem 80, and a re-enabled orphan would activate with its creator gone). Implementation: re-enabling an *entry* instantiates a *fresh fiber* – the entry is the identity across revisions, the fiber the identity of one enablement. Shortcuts the loader takes (config handed to the component, realm moved without reloading the provider) are justified by their *endpoint* (Theorem 80).

## 8. Obligations the theory places on an implementation

| Obligation | Source |
|---|---|
| Disposers must revert exactly their effect (author obligation, unchecked) | Def. 8 witness |
| Shared keys must be commutative (provider obligation, unchecked) | Def. 46 |
| Single provider per (key, realm) | O-Insert premise |
| Provider available only while Active | eq. 46 |
| Committed view fixed per episode, read by consumers | Lemma 59(2), Thm 70 |
| Out-of-service mark before any inverse runs | L-Leave |
| Unload waits for dependents (guard) | L-Unload |
| Iteration-boundary staleness check | L-Iter/L-Finish premise, Thm 71 |
| Failed fiber not retried until revised | §4.4 Failure |
| Report dependency cycles (≺ not acyclic ⇒ permanently inactive) | Thm 73 assumption, §6.5 |
