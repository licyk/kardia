# Cordis Loader (`@cordisjs/plugin-loader`)

> **Layer:** Implementation (`cordis/packages/loader/src/**`).
> Paper counterpart: §5.2.1 *Declarative configuration* and §4.4 *Configuration* – see [`../paper/07-implementation-in-paper.md`](../paper/07-implementation-in-paper.md#6-component-loader-52).
> File:line references are relative to `packages/loader/src/`.

## 1. Role

The loader turns a declarative **configuration tree** of *entries* into live fibers and keeps the two in step **bidirectionally**: config changes reconcile the fibers; a plugin that updates its own config (`ctx.fiber.update`) or disposes itself (`ctx.fiber.dispose()`) is written back to its entry. It provides the `loader` service and owns the in-memory root tree; the same machinery is reused by `group` and `include`.

```
root Context
 └─ Loader (service "loader", EntryTree, in-memory, commit() = no-op)
     └─ root EntryGroup (data: EntryOptions[])
         ├─ Entry "a"   plain plugin
         ├─ Entry "g"   @cordisjs/plugin-group, group: true  → Group (EntryGroup sharing the parent tree) → children
         └─ Entry "inc" @cordisjs/plugin-include              → Include (EntryTree #2, file-backed) → entries "inc:x"
```
Bootstrap (`core/bin.js`): create a root `Context`, set `baseUrl` to the cwd, `await ctx.plugin(Loader)`, then `ctx.loader.create({ name: '@cordisjs/plugin-include', config: { path: './cordis.yml' } })`.

## 2. Data model

### `EntryOptions` (`config/entry.ts:8-16`, + `isolate` from `config/isolate.ts:6-9`)
```ts
interface EntryOptions {
  id: string                          // local id in its tree; must not contain ':' (EntryTree.sep)
  name: string                        // plugin module specifier ("url" in the paper)
  config?: any                        // may contain expressions (except for groups)
  group?: boolean | null              // structural group: config must be EntryOptions[]
  disabled?: boolean | null | JsExpr  // the only field validated as expression-capable
  inject?: Inject | null              // array or { name: meta }; values may be expressions
  intercept?: Dict | null             // { service: meta }; values may be expressions
  isolate?: Dict<true | string> | null// service → private realm | shared realm label
}
```
`null` means "delete this key" when merging; unknown keys are tolerated (only known keys are validated). `sortKeys` gives a stable order (`id, name`, others alphabetically, `config` last) for file writes.

Mapping to Definition 81: `id` ✔, `url` → `name`, `isolate` ✔, `intercept` ✔, `config` ✔, `disabled` ✔ – plus `inject` and `group`, which the paper does not list.

### `Entry` (`config/entry.ts:55-68`)
| Field | Meaning |
|---|---|
| `ctx` | `loader.ctx.extend({[Entry.key]: this})`; its prototype is **re-pointed** to `parent.ctx` on every patch (live re-parenting when entries move) |
| `outerFiber?` | exists only when `inject` is non-empty: holds the entry's `inject` and parents the inner fiber |
| `innerFiber?` | the real plugin fiber |
| `parent: EntryGroup`, `options` | `options` is the **same object** as the element in `parent.data` |
| `subgroup?`, `subtree?` | set when the plugin constructs an `EntryGroup` (Group) or is an `EntryTree` (Include) |
| `inject` | resolved + evaluated inject |
| `_initTask?` | dedupe promise for (re)initialization |
| `_disposing` | marks loader-driven disposal (to tell it from self-dispose) |
| `_verdict?: boolean` | cached evaluation of `disabled` (`undefined` = undecided ⇒ disabled) |
| `_intercept` | live intercept table, mutated in place (`swap`) so running plugins see new metadata without restart |
| `realm?` | local realm (isolation) |

`id` is composite across include trees (`inc:mid:a`). `disabled` is `_verdict !== false || _parentDisabled()`; groups are never disabled themselves but pass their verdict to children.

### Two fibers per entry when `inject` is set (`config/entry.ts:36-46, 341-354`)
```ts
const outer = { name: 'loader:outer', async* apply(ctx) {
  const entry = ctx[Entry.key]
  const exports = await entry._import()   // re-import (HMR-reloaded module is picked up)
  yield () => {}                          // iteration boundary = checkpoint
  if (exports) entry._start(ctx, exports.plugin)
}}
```
The **outer fiber** carries the entry-level dependencies; the **inner fiber** is the actual plugin. If a dependency disappears during the import, the async-iterator guard stops at the `yield` and the plugin never starts (an L-Divert at an iteration boundary). Losing a dependency later unloads the outer fiber, which disposes the inner one as a child effect; when it returns, the outer fiber reloads and re-imports.

## 3. Core hooks installed by the loader (`index.ts:73-170`)

| Hook | Purpose |
|---|---|
| `internal/config` (prepend) | relocate `ValidationError`s to config-file positions when the config came *from the file* (identity check) |
| `internal/config` (append) | **interpolate expressions** in `config` before validation (skipped for groups) in the inner fiber's context |
| `internal/commit` | plugin self-update: write `config` back into `entry.options` and `tree.commit(...)` |
| `internal/update` (global) | log `reload plugin` |
| `internal/status` | outer fiber → `UNLOADING` resets `_verdict` (entry reads as disabled while unloading) |
| `internal/plugin` | tag new fibers with their entry; on disposal, detect a **genuine self-dispose** (not loader-driven, not parent teardown, not tree disposal) and persist `disabled: true` |

`Loader[Service.check]` implements `inject: { loader: { await: true } }`: the `loader` service reads as unavailable until all entry init tasks and fiber transitions have settled (`getTasks()`), giving a "configuration fully loaded" barrier.

## 4. Expressions (`config/utils.ts`)

* Representation `{ __jsExpr: string }`, produced by the YAML tag `!!js` or written in JSON. A whole value is an expression or not (no string templating).
* `evaluate(ctx, expr)` runs `new Function('ctx','expr','with (ctx) { return eval(expr) }')` – free identifiers resolve through the **context proxy**, i.e. normal inject rules apply.
* Evaluation contexts:

| Field | Evaluated in | When | On failure |
|---|---|---|---|
| `inject` values | parent group's ctx | every update | log, empty inject, disabled |
| `disabled` | outer fiber ctx (sees entry inject) | start / reconcile | log, disabled |
| `intercept` | outer fiber ctx | start / reconcile, if enabled | log, disabled |
| `config` | inner fiber ctx | each (re)load via `internal/config` | fiber FAILED |

* The `disabled` verdict is cached; re-evaluated on start, explicit update or outer reactivation.
* Errors are re-traced from V8 frames to `file:line:col` of the expression inside the config file.

## 5. Algorithms

### `Entry.update(options, create, force)` (`config/entry.ts:218-257`)
```
legacy = shallow copy
create ? adopt `options` by reference : merge(options, given)       // synchronous, before any await
changed = _resolveInject();    if failure → dispose; return
if _blocked() → dispose; return                                      // literal disabled / disabled ancestor
if changed || name changed || no live fibers → await init(dispose()) // REBUILD
diff = deep-unequal keys; if none && !force → return
emit 'loader/partial-dispose'(entry, legacy, true)
await _patchContext(diff)                                            // re-parent, isolate, intercept, config
```
`_patchContext` runs the `loader/patch-context` waterfall (the isolate plugin wraps it), re-parents `entry.ctx` and the intercept chain, then `_reconcile`: re-evaluate `disabled` and `intercept` (dispose the inner fiber if now disabled), and if `config` changed (or the entry is a group) call `innerFiber.update(config, noSave = true)` – the component decides whether to restart (paper: "config handed to the component").

Paper field dispatch vs. code:

| Field | Paper | Code |
|---|---|---|
| `id`, `url` | rebuild | `name` change ⇒ rebuild (id is the key; a different id is a different entry) |
| `isolate` | reassign realms | `loader/patch-context` isolate algorithm (§6) |
| `intercept` | in place | `_intercept` swapped in place, no reload |
| `config` | handed to component | `innerFiber.update(config, true)` → `internal/update` veto or restart |
| `disabled` | unload / reload | `_verdict` → dispose inner fiber / `outerFiber.restart()` or `init()` |
| `inject` | (n/a) | rebuild (outer fiber's specification changes) |

### `init` / `_init` (`config/entry.ts:311-358`) – concurrency-safe (re)initialization
`_initTask ??= _init(disposeTask)` makes concurrent callers join one task. `_init` awaits the pending disposal, then re-checks after **every** await (name unchanged? still registered? not blocked? no fiber created meanwhile?), imports the module, patches the context, releases `_initTask` *before* creating fibers (their `apply` may trigger another init), and creates the outer fiber (if `inject`) or starts the plugin directly. `rebuild.spec.ts` / `evaluate.spec.ts` cover renames during import, removal during disposal, a plugin renaming its own entry, etc.

### `EntryTree` (`config/tree.ts:71-261`) and `EntryGroup` (`config/group.ts`)
* `EntryTree`: `ctx`, `root: EntryGroup`, `store: Dict<Entry>` by local id; `create/remove/update(id, options, parent?, position?)` (runtime API, deep-validated before anything is committed; supports moves within and across trees), `resolve(compositeId)`, `getTasks()/await()`, abstract `commit(change)`; `EntryChange { id, group, from?, options?, legacy? }` encodes create / remove / update / move.
* `EntryGroup.update(config)` – **keyed reconciliation**: union of old and new ids; for each, in parallel (`Promise.all`), `create()` (adopt/update – forced) or `remove()`. Anonymous entries get a unique key so each is created with a fresh random id. One bad entry does not stop others.
* `Group` (the `@cordisjs/plugin-group` plugin) extends `EntryGroup`, **shares the parent tree's store**, and vetoes restarts with `ctx.on('internal/update', config => this.update(config))` – this is the paper's "group reconciliation and entry update recurse together down the tree".
* Unregister-before-dispose ordering in `remove`/`_reject` is what lets self-dispose detection distinguish loader-driven disposal.

### Module import (`config/tree.ts:198-246`, `resolve.ts`) – Node-specific
Imports through Node's internal ESM loader (via `loader-utils`) with the tree's `baseUrl` as parent URL, so specifiers resolve relative to the config file; bare specifiers are resolved via a generated helper module `<scope>/.cordis/resolve.mjs` so they resolve in the *project's* dependency graph. `ERR_MODULE_NOT_FOUND` for the requested module becomes `ModuleNotFoundError` located at the entry's `name`; a missing *transitive* dependency stays the plugin's own error.

## 6. Managed realms (`config/isolate.ts`)

* `isolate: { svc: true }` → **LocalRealm** owned by the entry (symbol `svc#<localId>`), travels with the entry.
* `isolate: { svc: 'label' }` → **GlobalRealm** shared by every entry using that label (symbol `svc@label`); garbage-collected on `loader/partial-dispose` once no entry references it.
* Each entry gets its own isolate table (`Object.create(parent table)`) at `loader/entry-init`.

The `loader/patch-context` listener implements Algorithm 7:
1. compute the new table (own keys from `options.isolate`, inheriting the parent group's table);
2. for each name whose realm symbol changes, stamp a fresh **delimiter tag** `entry.ctx[delim(name)]` (marks "contexts inside this entry", inherited by descendants) and record `[oldSym, newSym, entryTag, providerTag]` for the provider found in the store;
3. re-parent and swap the table **in place** (identity preserved for descendants);
4. run the rest of the patch (`next()`);
5. **migrate** a binding whose provider is inside the entry (tags equal) to the new symbol if it is free – the provider keeps the same `Impl` (same uid) and **does not restart**; if the target is occupied the move is *blocked* and `Impl.isolate` keeps tracking where it really lives;
6. `reflect.notify(names, filter)` with filter `ctx.isolate[name] ∈ {old, new} && (entryTag === ctx[delim]) !== (entryTag === providerTag)` – only fibers whose inside/outside relation differs from the provider's are re-checked;
7. drop delimiter tags the entry does not itself need.

**Likely bug (verified in source):** in the realm GC listener (`config/isolate.ts:150-153`) the inner loop uses `return` when an entry still references the realm, which exits the whole listener and skips GC for the remaining names of the same `legacy.isolate`; `continue` (of the outer loop) was almost certainly intended. Effect: a stale global realm may survive until a later partial-dispose.

## 7. Validation (`config/utils.ts:159-209`)
`validateOptions(options, sep, deep)` returns `Issue { message, path }[]`: object shape; `id` string without `:`; `name` required string; `group` boolean; `disabled` boolean or expression; group `config` must be an array; `inject` array of strings or object; `intercept`/`isolate` objects (isolate values `true` or string). Deep for the runtime API, shallow per entry for file input (children are validated when their group creates them). This is the "validate once at the entry boundary" discipline.

## 8. Concurrency model
Single-threaded cooperative JS. All coordination uses promise de-duplication (`_initTask ??=`), latest-wins slots, flags, and the guarantee that synchronous code between `await`s is atomic. `EntryGroup.update` reconciles children concurrently. The loader never awaits fiber activation where it could deadlock (`init` settles fibers in a detached promise). The paper's justification (Theorem 70: entries can be instantiated together; Theorem 80: the order does not matter) is what makes concurrent module loading safe.
