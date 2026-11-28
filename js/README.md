# @kardiajs

The TypeScript side of [kardia](https://github.com/licyk/kardia): the same plugin model as the Rust crates, and the bridge that connects a TypeScript runtime, such as a Tauri webview, to a Rust kardia app. Guide: [TypeScript and Tauri](https://kardia-dev.netlify.app/docs/guide-typescript).

| Package | Contents |
|---|---|
| [`@kardiajs/core`](packages/core/) | Contexts, fibers, revertible effects, reactive services, events and logging |
| [`@kardiajs/timer`](packages/timer/) | Timeouts, intervals, sleep, throttle and debounce that end with their plugin |
| [`@kardiajs/bridge`](packages/bridge/) | The client side of the bridge to a Rust app running `kardia-bridge` |
| [`@kardiajs/tauri`](packages/tauri/) | The bridge's transport over one Tauri command and a `Channel` |
| [`@kardiajs/vue`](packages/vue/) | Vue integration: extensions contribute components, views and routes to a Vue app, and they leave with their plugin |

## Development

Version 0.3.0 changes what the bridge reports about a session: `@kardiajs/bridge` now describes the identity a session actually has, instead of the one the client opened it with, so a host that replaces or restores a session is reported as it is. See [the bridge](https://kardia-dev.netlify.app/docs/guide-typescript#bridge).

Version 0.2.0 added configurable JS plugin recovery to `core` and callback failure isolation to `timer`. `fiber.setRecovery()` selects manual (default), bounded retry or host-defined handling. `fiber.fail(error)` reports an unusable instance; ordinary timer callback errors are logged without forcing a restart. Disposal waits for in-flight timer callbacks. See [recovery and boundaries](https://kardia-dev.netlify.app/docs/guide-typescript#recovery).

The bridge continues using explicitly exposed JSON services and events; Rust Server plugins have no duplicate JS packages. The existing JS workflow checks packages and real Rust/JS recovery on Linux, Windows and macOS. Upstream Server comparisons remain in the separate, manually triggered `Server reference` workflow.

This is a Bun workspace, outside the Cargo workspace.

```sh
bun install
bun run check       # TypeScript, over every package's sources and tests
bun run test        # Vitest
bun run test:e2e    # builds kardia-bridge's stdio host example, then drives it from the client
bun run build       # each package's lib/, in dependency order
```

- The packages share one version, apart from the crates'. `cargo xtask js-version <patch|minor|major>` bumps it, and `cargo xtask js-tag --push` tags `js-v<version>`, which starts `.github/workflows/npm-release.yml` (npm trusted publishing).
- Inside a package, modules import each other as `#/<module>` (subpath imports, mapped by each `package.json`'s `imports`): Node, TypeScript and bundlers resolve them in the published `lib/`, and the type check and tests resolve them to the sources through the `source` condition.
- `scripts/build.ts` copies the repository's `LICENSE` and this directory's `NOTICE` into each package before building it.

[Contributing](https://kardia-dev.netlify.app/docs/development-contributing#js) has the details, including the one-time npm setup.

## License

GPL-3.0-only, like the crates. The packages are a port of [Cordis](https://github.com/cordiverse/cordis), which is MIT-licensed; [`NOTICE`](NOTICE) keeps its notice.
