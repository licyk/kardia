# @kardiajs

The TypeScript side of [kardia](https://github.com/licyk/kardia): the same plugin model as the Rust crates, and the bridge that connects a TypeScript runtime, such as a Tauri webview, to a Rust kardia app. Guide: [TypeScript and Tauri](https://kardia-dev.netlify.app/docs/guide-typescript).

| Package | Contents |
|---|---|
| [`@kardiajs/core`](packages/core/) | Contexts, fibers, revertible effects, reactive services, events and logging |
| [`@kardiajs/timer`](packages/timer/) | Timeouts, intervals, sleep, throttle and debounce that end with their plugin |
| [`@kardiajs/bridge`](packages/bridge/) | The client side of the bridge to a Rust app running `kardia-bridge` |
| [`@kardiajs/tauri`](packages/tauri/) | The bridge's transport over one Tauri command and a `Channel` |

## Development

This is a Bun workspace, outside the Cargo workspace.

```sh
bun install
bun run check       # TypeScript, over every package's sources and tests
bun run test        # Vitest
bun run test:e2e    # builds kardia-bridge's stdio host example, then drives it from the client
bun run build       # each package's lib/, in dependency order
```

- The packages share one version, apart from the crates'. `cargo xtask js-version <patch|minor|major>` bumps it, and `cargo xtask js-tag --push` tags `js-v<version>`, which starts `.github/workflows/npm-release.yml` (npm trusted publishing).
- Relative imports name `.js` files, so `tsc` emits working JavaScript and declarations as they are.
- `scripts/build.ts` copies the repository's `LICENSE` and this directory's `NOTICE` into each package before building it.

[Contributing](https://kardia-dev.netlify.app/docs/development-contributing#js) has the details, including the one-time npm setup.

## License

GPL-3.0-only, like the crates. The packages are a port of [Cordis](https://github.com/cordiverse/cordis), which is MIT-licensed; [`NOTICE`](NOTICE) keeps its notice.
