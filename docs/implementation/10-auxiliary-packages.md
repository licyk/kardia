# Cordis Auxiliary Packages: timer, logger-console, fs-node, package-manager, loader-utils, create

> **Layer:** Implementation. These packages are not described in the paper; they show how ordinary services are built on the core primitives. Common theme: **every resource is registered through `ctx.effect` on the *calling* fiber** (found via the service tracker, see [05](05-services-and-traceability.md)), so it is released when the caller unloads – no per-consumer bookkeeping in the service.

## 1. `@cordisjs/plugin-timer` (`timer/src/index.ts`)
Service `timer`, mixed into the context as `ctx.timeout / interval / throttle / debounce / setTimeout / setInterval`.

| API | Semantics |
|---|---|
| `timeout(cb, ms) → dispose` | effect around `setTimeout`; when it fires it first disposes itself (removes the effect from the fiber), then calls `cb` |
| `timeout(ms) → Promise<void>` | sleep; disposing the fiber rejects it with `Error('Context has been disposed')`; the effect is removed when the promise settles |
| `interval(cb, ms) → dispose` | effect around `setInterval` |
| `interval(ms) → AsyncIterableIterator` | each tick resolves only the oldest pending `next()`; ticks without a waiter are dropped; `return()`/`throw()` semantics of async iterators; fiber disposal rejects pending reads |
| `throttle(cb, ms, noTrailing?)` | leading call, trailing call with latest args unless `noTrailing`; `.dispose()` |
| `debounce(cb, ms)` | each call restarts the timer; `.dispose()` |

## 2. `@cordisjs/plugin-logger-console` (`shared.ts`, `index.ts`, `browser.ts`)
A class plugin (`name = 'logger-console'`) whose constructor calls `ctx.logger.exporter(this)` – the exporter lives exactly as long as the plugin. Config: `colors` (Node default from `supports-color`), `maxLength`, per-name `levels`, `showDiff`, `showTime` template, `label { width, margin, align }`. Renders `[E]/[W]/[I]/[D]` prefix, hashed-colour label, aligned continuation lines, optional `+Nms` diff; `%o` uses `util.inspect`. Browser build delegates to `console.*`.

## 3. `@cordisjs/plugin-fs-node` (`index.ts`, `watcher.ts`)
Service `fs`, currently only `watch(path, { filter }) → Watcher`.
* Path resolution relative to the **caller's** `ctx.baseUrl`; `file:` URLs accepted; Windows drive-relative forms rejected.
* A single file is watched **through its parent directory** (`depth: 0`) so atomic rename-based writes do not lose the watch.
* The `Watcher` is an async effect wrapping chokidar (`ignoreInitial`), registered after the initial scan; only `change` events are forwarded; `ready` replays asynchronously to late listeners; snapshot dispatch; listener errors go to `error` listeners (whose own errors are only logged).
* Filter = gitignore-like rules, **last match wins**, tested against the entry *or any ancestor*; a directory is still traversed if a later including rule's literal base is related; patterns leaving the root are rejected.

## 4. `@cordisjs/plugin-package-manager` (`index.ts`, `agent.ts`, `manifest.ts`, `backup.ts`, `process.ts`, `state.ts`)
Service `packageManager`: transactional dependency installs driving npm / pnpm / yarn classic / yarn berry (hoisted `node_modules` only).
* `install(changes, { signal })` is serialized: detect agent from `package.json#packageManager` (exact version, verified by `--version`), require a lockfile, check the layout, snapshot install state + back up manifest/lockfiles/rc files (mode- and symlink-preserving), edit `package.json` (preserving indentation/EOL/sort order), run the agent with an **inactivity timeout** and abort support, then diff the install state (`added/removed/updated` with real paths).
* On failure: restore files and run a **non-cancellable recovery install**; if that fails too, throw an `AggregateError`.
* Processes are spawned in their own process group (POSIX) and the whole tree is killed (SIGTERM → SIGKILL; `taskkill /T` on Windows). The environment is scrubbed of variables leaked by an outer `yarn run`.
* Real paths use Node's JS `realpath` algorithm so they match module URLs (intended to feed hmr roots; not wired up in the repo).

## 5. `@cordisjs/loader-utils` (`index.ts`, `internal.ts`)
`unwrapExports` (ESM/esbuild interop) and `ModuleLoader.fromInternal()` – access to Node's internal cascaded ESM loader (shape detected by API presence across Node 22–24.12+). Pure Node glue; **no Rust counterpart**.

## 6. `create-cordis` (`create/src/index.ts`, `bin.ts`)
Standalone scaffolding CLI (does not use core): prompt for a name, prepare the directory, fetch the template package (`@cordisjs/boilerplate`) tarball from the configured npm registry, extract with `strip: 1`, optionally stage a pinned Yarn binary (five documented rules), rewrite `package.json`, optionally `git init`, install and start. Errors print a fixed message and `process.exit(1)` (tests run it as a child process). The `--mirror` flag is parsed but unused.

## 7. Takeaways for a port
* timer, fs, logger exporters are thin wrappers proving that **"lifetime by effect" + caller attribution** is enough to make arbitrary resources revertible – exactly the paper's §6.1 acquisition-inside-the-boundary pattern.
* package-manager and create are Node-ecosystem tools; for a Rust framework they are optional or must be retargeted (e.g. Cargo / WASM component packages).
