# Cordis Repository Overview

> **Layer:** Implementation. The TypeScript code base at `cordis/` (a separate git repository, ignored by the outer repo's `.gitignore`; HEAD `6663348 fix(fs-node): reject drive-relative paths on Windows (#285)`). `cordis` core version `4.0.0-rc.12` – the paper's "Cordis v4".

## 1. Packages

~7.5k lines of TypeScript source, ~13.8k lines of TypeScript tests.

| Package (dir) | npm name | Version | Src LOC | Role | Paper section |
|---|---|---|---|---|---|
| `core` | `cordis` | 4.0.0-rc.12 | ~2,030 | Context, effects, fibers, registry, reflection, services, events, logger | §3, §5.1 |
| `loader` | `@cordisjs/plugin-loader` | 1.0.0-rc.9 | ~1,430 | Declarative entries, reconciliation, expressions, managed realms | §5.2.1 |
| `group` | `@cordisjs/plugin-group` | 1.0.0 | 3 | Re-export of the loader's `Group` | §5.2.1 |
| `include` | `@cordisjs/plugin-include` | 1.2.0 | ~1,210 | File-backed (YAML/JSON) subtrees with two-way sync | §5.2.1 |
| `hmr` | `@cordisjs/plugin-hmr` | 1.1.2 | ~790 | Transactional hot module replacement | §5.2.2 |
| `loader-utils` | `@cordisjs/loader-utils` | 0.1.0 | ~170 | Node internal ESM loader glue | – |
| `timer` | `@cordisjs/plugin-timer` | 1.1.4 | ~150 | Fiber-scoped timers | – |
| `logger-console` | `@cordisjs/plugin-logger-console` | 1.0.0 | ~140 | Console log exporter | – |
| `fs-node` | `@cordisjs/plugin-fs-node` | 0.1.0 | ~240 | `fs.watch` service (chokidar) | – |
| `package-manager` | `@cordisjs/plugin-package-manager` | 0.1.0 | ~960 | Transactional npm/pnpm/yarn installs | – |
| `create` | `create-cordis` | 0.3.2 | ~400 | Project scaffolding CLI | – |

Runtime dependencies of core are only `cosmokit` (utilities) and `@standard-schema/spec` (config validation interface). Plugins peer-depend on `cordis`.

### Dependency graph (runtime)
```
cordis (core) ◄── loader ◄── group
      ▲  ▲  ▲        ▲  ▲
      │  │  │        │  └── include ──(optional fs service)──► fs-node
      │  │  │        └── (imports via) loader-utils ◄── hmr ──(optional fs service)──► fs-node
      │  │  └── timer, logger-console, fs-node, package-manager
      │  └── core/bin.js: Context + Loader + include('./cordis.yml')
create (standalone)
```

## 2. Tooling
* Yarn 4 workspaces (`external/*`, `packages/*`), ESM only, TypeScript executed directly via a `tsx` fork.
* `yakumo` build orchestrator (`yakumo.yml`: `yakumo`, `yakumo-tsc`, `yakumo-esbuild`, `yakumo-vitest`): `build` = esbuild bundles + `tsc` declarations; `test` = vitest; coverage variants.
* Vitest with `pool: 'forks'` and `--expose-internals` (needed by hmr; forked processes isolate module-cache mutation per test file).
* ESLint (`@cordisjs/eslint-config`).
* CI (`.github/workflows/build.yml`): lint (Node 26), build + publish on `main`, tests on Ubuntu/Windows/macOS × Node 24/26.

## 3. Core source map (`packages/core/src`)
| File | Lines | Contents | Doc |
|---|---|---|---|
| `context.ts` | 78 | `Context` class, derivation (`extend`, `isolate`, `intercept`) | [02](02-context-and-reflection.md) |
| `reflect.ts` | 324 | `ReflectService`: store, `provide`, `get/set`, `notify`, accessors, mixins, Proxy handler, `locate` | [02](02-context-and-reflection.md) |
| `fiber.ts` | 553 | `Fiber`, effect engine, lifecycle state machine, update/restart, `ValidationError` | [03](03-effects.md), [04](04-fiber-lifecycle.md) |
| `registry.ts` | 246 | Plugin types, `Inject`, `RegistryService` (`plugin`, `inject`, `replace`) | [04](04-fiber-lifecycle.md) |
| `service.ts` | 83 | `Service` base class, interception resolution | [05](05-services-and-traceability.md) |
| `utils.ts` | 285 | `DisposableList`, symbols, traceable proxies, error stack composition | [03](03-effects.md), [05](05-services-and-traceability.md) |
| `events.ts` | 224 | `EventsService`, dispatch modes, internal events | [06](06-events-and-logger.md) |
| `logger.ts` | 233 | `Logger`, `LoggerService`, formatting | [06](06-events-and-logger.md) |

## 4. Test suite as specification
The tests are the most precise specification of intended behaviour, and should be treated as the acceptance suite for a port. Highlights:
* core: `fiber.spec` (inertia locks, failure, update), `update.spec` (concurrent updates), `service-dispose.spec` (guard and drain ordering), `isolate.spec`, `shadow.spec`, `associate.spec`, `replace.spec`, `dispose.spec` (effect shapes), `events.spec`;
* loader: `evaluate.spec`, `rebuild.spec` (interleavings), `isolate*.spec`, `commit.spec`, `validate.spec`;
* include: `sync.spec` (journal/patch/merge matrix), `write.spec`, `location.spec`;
* hmr: `reconcile.spec`, `index.spec`, `watch.spec`.

## 5. Document index for this layer
1. this file
2. [Context and reflection](02-context-and-reflection.md)
3. [Effects](03-effects.md)
4. [Fiber lifecycle and registry](04-fiber-lifecycle.md)
5. [Services and traceability](05-services-and-traceability.md)
6. [Events and logger](06-events-and-logger.md)
7. [Loader](07-loader.md)
8. [Include and group](08-include-and-group.md)
9. [HMR](09-hmr.md)
10. [Auxiliary packages](10-auxiliary-packages.md)
11. [Paper vs. implementation](11-paper-vs-implementation.md)
