# Cordis Include (`@cordisjs/plugin-include`) and Group (`@cordisjs/plugin-group`)

> **Layer:** Implementation (`cordis/packages/include/src/{index,journal,patch}.ts`, `packages/group/src/index.ts`).
> Paper counterpart: §5.2.1 ("`@cordisjs/include` loads an external configuration file (YAML or JSON) and grafts its entries in as a nested subtree"). The paper does not describe the synchronization machinery below; it is an implementation contribution.

## 1. Group
`packages/group/src/index.ts` only re-exports `Group` from the loader (see [07-loader](07-loader.md#entrytree-configtreets71-261-and-entrygroup-configgroupts)). Its config is the list of child entries; config updates are absorbed in place as a keyed diff.

## 2. Include – a file-backed `EntryTree`

`class Include extends EntryTree`, `inject = ['loader']`, used as `{ name: '@cordisjs/plugin-include', config: { path, initial?, patches?, enableLogs?, watch? } }`. It is a class plugin whose async-generator `[Service.init]` reads the file, yields `() => this.stop()`, and applies the tree. Its entries' composite ids are `<includeEntryId>:<localId>`; includes nest.

* `path` resolves against the parent tree's `baseUrl`; the include's own `ctx.baseUrl` becomes the file's directory (nested names/includes resolve relative to the file).
* `initial` is written if the file does not exist (ENOENT only); an unparsable existing file is **never overwritten**.
* `watch: true` → `ctx.inject(['fs'], ctx => ctx.fs.watch(filename).on('change', () => this.refresh()))`.
* An `internal/update` listener reconfigures in place (new `patches`) unless `path`/`watch` changed (then full restart).

### State
`filename`, `type` (`application/json` | `application/yaml` by extension), `cache?: { content, data (parsed, ids assigned, unpatched), syntax? }`, `journal: Map<id, JournalRecord>` (tree-side changes not yet in the file), `index?: PatchIndex` (ownership), `anonymous` (ids invented for id-less file entries), IO flags `dirtyRead/dirtyWrite`, `_draining`, `_pendingTree`/`_applyTask` (latest-wins application).

## 3. The IO loop (`index.ts:365-397`)
Single-flight `_drain` runs `while (dirtyRead || dirtyWrite) { read-if-dirty; write-if-dirty }`. Read errors are **warnings** (half-written files are routine while editing) and stop the loop without writing (file state unknown); write errors are logged and the loop continues. `commit(change)` (from the tree) records into the journal, sets `dirtyWrite`, drains. `refresh()` (watcher) sets `dirtyRead`.

### `_read()`
1. read; identical content ⇒ no-op (also suppresses watcher feedback from own writes);
2. parse YAML (core schema regardless of `%YAML`, custom tag `!!js`, **errors and warnings both fatal**) or JSON; map issues to `file:line:col` (1-based, **UTF-16 columns**);
3. validate top-level array / object entries; on failure keep the previous tree;
4. assign ids to anonymous entries (stable across reads: reuse previous id if same parent and deep-equal options);
5. if not the first read: three-way **reconcile** of the journal against old/new file states – **file wins** on conflict, each conflict logged;
6. cache, schedule apply; if the journal is non-empty, schedule a write.

### `_write()`
Re-read; if the file changed externally, merge first. Snapshot-and-swap the journal; `routeJournal` sends each change to its **owner** – the file data, a patch in the include's own config, or an entry embedded in a patch. Patch changes are persisted by `this.ctx.fiber.update(newConfig)` (not awaited) which flows through `internal/commit` into the **parent** tree's file. File changes are dumped (**whole-file regeneration: comments and formatting are lost**) and written atomically: temp file `.<base>.<pid>.<seq>.tmp` in the same directory, stale-content check before **every** rename attempt, rename with retries on EACCES/EPERM/EBUSY (11 attempts, linear backoff); read-only files keep changes in memory with a warning.

### Tree application
`applyPatches(cache.data, patches)` (fresh clone every time, so removing a patch reverts) → `applyJournal(data, journal)` (overlay unwritten runtime changes) → latest-wins `root.update(tree)` → `EntryGroup` keyed reconciliation.

## 4. Journal (`journal.ts`) – pure functions
```ts
type JournalRecord =
  | { kind: 'remove' }
  | { kind: 'upsert'; created: boolean; parent: string | null | undefined; position: number; changes: Dict }
```
`record` (deltas captured at report time), `mergeRecords` (remove∘created-upsert = nothing; upsert after remove ⇒ `created:false`; changes merged newer-wins), `flatten`, `applyChanges`, `detach`, `place`, `applyJournal`, and `reconcile(journal, base, theirs, fileOwned)` – the three-way merge where conflicting keys/moves/removals are dropped from the journal ("file wins").

## 5. Patches (`patch.ts`)
* **insert patch** `{ id?, insert: [...] }` – append entries into a group (or root).
* **override patch** `{ id, name?, ...keys }` – override keys of an existing entry; `name`, if given, must match (guard against patching a different plugin); overriding a group's `config` replaces its child list.
* `applyPatches` builds a `PatchIndex` recording the **owner** of every entry and key: `file`, `patch{index}`, or `embedded{index, key}`; later patches can address entries inserted by earlier ones.
* `routeJournal` writes each runtime edit back to its owner (patch-owned keys → the patch; entries inserted by a patch → inside the patch; the rest → the file).

## 6. Source locations
`_route(id, path)` finds where a value is written (file, patch, embedded) and delegates up through nested includes, producing virtual stack frames like `at a (inner.yml:1:3)` / `at inc (middle.yml:1:3)`. `walk` descends the YAML AST (resolving aliases to anchors); `seekExpr`/`scanScalar` map an offset inside an expression to a file offset (decoding quoted-scalar escapes).

## 7. Errors
`ConfigFileError(stage: 'read'|'parse'|'validate', path, cause, issues[])`, internal `StaleWriteError`; loader-side `EntryValidationError`, `ModuleNotFoundError`, relocated `ValidationError`.

## 8. Design notes
* The include is the clearest example of the paper's *bidirectional* binding between configuration and fibers: runtime changes (plugin self-update/self-disable, API edits) flow back into files, file edits flow into fibers, and conflicts are resolved deterministically.
* Correctness relies on confluence (Theorem 80): the include may apply intermediate trees, skip them (latest-wins) or apply them in a different order – the quiescent state depends only on the final tree.
* The synchronization core (journal, patch, reconcile, routing, id assignment) is **pure data transformation** over entry lists and is the most portable part of the code base.
