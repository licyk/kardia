# Cordis Hot Module Replacement (`@cordisjs/plugin-hmr`)

> **Layer:** Implementation (`cordis/packages/hmr/src/index.ts`, `error.ts`; plus `core/src/registry.ts:185-209` and `core/src/fiber.ts:401-416`).
> Paper counterpart: §5.2.2, Algorithms 8–10 – see [`../paper/07-implementation-in-paper.md`](../paper/07-implementation-in-paper.md#hot-module-replacement-522).
> **The implementation differs substantially from the paper's Algorithm 10** (see §6).

## 1. Integration
* `class Hmr extends Service`, service name `hmr`. Config: `base?` (base directory, default `ctx.baseUrl`), `watch.enabled` (default false), `watch.files` (glob filter, default `['.', '!**/node_modules', '!**/.*']`), `watch.debounce` (100 ms).
* Requires Node's internal ESM loader (`loader-utils` → `ModuleLoader.fromInternal()`, needs `--expose-internals` or the `node-addon-require-builtin` addon); the constructor throws otherwise (fiber FAILED, no service).
* Watching is an optional nested plugin `ctx.inject(['fs'], ...)` – active only while an `fs` service exists.
* `[Service.init]` records **host modules**: the import closure of `process.argv[1]` (the main program, core, loader…), which can only change by restarting the process.
* No loader-specific code: the loader imports plugins through the same internal ESM loader, so their modules are in the cache hmr inspects; hmr replaces plugins at the **registry** level and loader entries keep their fibers.

## 2. Module graph
hmr keeps **no graph of its own**; it reads the live graph from Node's ESM `loadCache` (`ModuleJob.linked`) and the CJS `require.cache` each time (`_edges`, `closure`). Builtins (`node:`) are skipped; native addons (`.node`) cannot be reloaded.

## 3. `reconcile(callback)` – one transaction (`index.ts:309-699`)

Transactions are serialized on a promise queue; re-entrant calls from inside a transaction (callback or a plugin `apply` during stage 3) are rejected using `AsyncLocalStorage`.

1. **Roots** – `await callback()` → absolute paths (validated).
2. **Sweep** – evict *failed* ModuleJobs from the cache (Node keeps them and rethrows on every import), per type slot.
3. **Match** – loaded modules whose path has a root as an ancestor (whole-segment match). Host modules / native addons → `skippedHostModules` (reported only).
4. **Analyze** – reverse edges (excluding edges from host modules); `accepted` = changed modules plus everything that transitively imports them; `declined` = host ∪ the rest. *(The paper's Algorithm 8 is a fixed point over forward imports with cycle-default-declined; the code is a simpler reverse-reachability worklist.)*
5. **Discover** – for accepted modules, unwrap exports and keep those that are registered plugins; **group by runtime** (several modules can export the same plugin).
6. **Stale plugins** – per runtime: the closure of its exporters through accepted modules; the module to re-import is the **outermost exporter** (keeps the entry module's `inject`/`Config` metadata).
7. **Backup and evict** every evicted URL (whole multi-slot ESM entry + CJS module).
8. **Stage 1 – import**, sequentially per stale plugin: re-import, check that the exports are a valid plugin (`registry.resolve`). *All fallible validation happens here, while only the module cache has been mutated*, so later stages need no rollback. No cache entry is restored before every plugin has been imported.
9. **Partition into units** with union-find: two attempts join a unit if their **old ∪ new** closures share an evicted module (two plugins sharing a changed dependency must not end up with different instances of it).
10. **Abort/commit** – units with any failed import are aborted and their modules restored; orphaned evicted modules are restored with aborted units they overlap (unless they reach a committed unit); stage-1-loaded modules linked against restored modules are evicted.
11. **Stage 2 – replace (synchronous)**: `ctx.registry.replace(oldPlugin, newPlugin)` per committed attempt. `replace` aliases the old plugin key to the new one (later `ctx.plugin(old)` instantiates the new code) and calls `fiber._replace(newRuntime, newInject)` on each fiber: re-check injections, clear errors, bump revision ⇒ **the same fiber object unloads and reloads in place** (identity, parent, config, loader entry preserved).
12. **Stage 3 – settle (concurrent)**: `await Promise.all(fibers.map(f => f.await().catch(noop)))`. **No rollback**: a plugin whose new `apply` throws stays FAILED (as on a cold start) and is retried on the next change.

Result: `{ settledFibers, importFailures: { urls, error }[], skippedHostModules }`.

### State preservation
Kept: fiber objects/uids, parents, configs, loader entries, services of *other* fibers. Lost: all effects of the old instance (disposed by the normal unload path, which waits for dependents first – the new instance never overlaps the old one) and module-level state of re-imported modules. Unrelated plugins are untouched; different fibers reload concurrently (a slow drain does not delay a fast plugin).

## 4. Watch mode (`index.ts:135-186`)
`ctx.fs.watch(baseDir, { filter }).on('change', f => …)` → debounce → serialized `flush` reconciling `retained ∪ stashed`. Host-module changes are warned about; files of failed units are **retained** and retried together with the next change (a broken file plus its dependency are retried together when only one is repaired). Only `change` events exist; a new file matters once a changed importer imports it.

## 5. `error.ts`
Pretty-prints esbuild `BuildFailure`s (tsx transpile errors) with `@babel/code-frame`.

## 6. Paper vs. code

| Aspect | Paper (Algorithms 8–10) | Code |
|---|---|---|
| Classification | forward fixed point; cycles default to declined | reverse-reachability from changed modules; host modules declined |
| Stale detection | per *entry* (`entry.url`) | per *plugin runtime* (any registered plugin exported by an accepted module); loader-agnostic |
| Swap | `entry.fiber.dispose(); entry.fiber ← ctx.use(import(url), config)` | `registry.replace` → same fiber object reloads in place |
| Failure handling | restore caches and rebuild *every* stale entry from backup, rethrow | validation in stage 1; failing **units** (union-find) aborted and restored, others committed; failures of the new `apply` stay FAILED (no rollback) |
| Atomicity | all-or-nothing | per dependency-connected unit |
| Re-entrancy | not discussed | serialized queue + AsyncLocalStorage rejection |

The code's design is arguably stronger: commit/abort is per unit, and the "point of no return" is placed after the only fallible step that can be cleanly reverted.

## 7. What is Node-specific
Everything in §2 and steps 2, 7–8, 10 depends on Node's module caches and internals; the *algorithm* (match → accept → group by runtime → stage-1 validation → union-find units → replace in place → settle) is portable once a host-owned module registry exists. See the Rust plan §10.
