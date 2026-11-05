# kardia Documentation

This directory holds the analysis that comes before a Rust re-implementation of **Cordis**. Cordis is the meta-framework of *spatiotemporal composability* described in the paper [`2608.25512v1.pdf`](2608.25512v1.pdf) (*A Programming Paradigm for Spatiotemporal Composability*, Shi, Zhang & Cui). The documentation has three layers, and every document names its layer at the top:

| Layer | What it describes | Where |
|---|---|---|
| **Theory** | What the paper defines, proves and prescribes | [`paper/`](paper/) |
| **Implementation** | What the TypeScript code in `../cordis/` actually does (Cordis v4, `cordis@4.0.0-rc.12`) | [`implementation/`](implementation/) |
| **Proposal** | How to rebuild it in Rust | [`rust-implementation-plan.md`](rust-implementation-plan.md) |

Where the paper and the code differ, the difference is recorded in [`implementation/11-paper-vs-implementation.md`](implementation/11-paper-vs-implementation.md). The Rust plan says which side it follows at each such point.

## Paper analysis (`paper/`)
1. [Overview, motivation, contributions, glossary](paper/01-overview.md)
2. [Revertible effects (§3.1)](paper/02-revertible-effects.md)
3. [Reactive coeffects, isolation, interception (§3.2)](paper/03-reactive-coeffects.md)
4. [Context paradigm, observational equivalence, independence (§3.3–3.4)](paper/04-context-paradigm.md)
5. [The calculus: components, fibers, the nine rules (§4.1–4.2)](paper/05-calculus.md)
6. [Metatheory and extensions (§4.3–4.4)](paper/06-metatheory.md)
7. [Implementation as described in the paper: Algorithms 1–10, loader, HMR, Koishi (§5)](paper/07-implementation-in-paper.md)
8. [Discussion, related work, conclusion (§6–8)](paper/08-discussion-and-related-work.md)

## Cordis code analysis (`implementation/`)
1. [Repository overview, packages, tooling](implementation/01-repository-overview.md)
2. [Context, reflection, coeffect resolution](implementation/02-context-and-reflection.md)
3. [Effect tracking](implementation/03-effects.md)
4. [Fibers, registry, lifecycle state machine](implementation/04-fiber-lifecycle.md)
5. [Services, traceable proxies, caller attribution](implementation/05-services-and-traceability.md)
6. [Events and logger](implementation/06-events-and-logger.md)
7. [Loader](implementation/07-loader.md)
8. [Include and group](implementation/08-include-and-group.md)
9. [Hot module replacement](implementation/09-hmr.md)
10. [Auxiliary packages (timer, logger-console, fs-node, package-manager, loader-utils, create)](implementation/10-auxiliary-packages.md)
11. [Paper vs. implementation: correspondences, deviations, defects](implementation/11-paper-vs-implementation.md)

## Rust plan
* [Rust implementation plan](rust-implementation-plan.md): architecture, crate layout, concurrency model, data structures, APIs, dependencies, phases, testing strategy, risks, and (§15) the implementation status in [`../crates/`](../crates/).

## Suggested reading order
* For the ideas: `paper/01` → `paper/05` → `paper/07`.
* For the port: `implementation/04` → `implementation/11` → `rust-implementation-plan.md`.
