# kardia

[English](README.md) | [简体中文](README.zh-CN.md)

[![CI](https://github.com/licyk/kardia/actions/workflows/ci.yml/badge.svg)](https://github.com/licyk/kardia/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/kardia.svg)](https://crates.io/crates/kardia)
[![docs.rs](https://img.shields.io/docsrs/kardia)](https://docs.rs/kardia)
[![License: GPL-3.0-only](https://img.shields.io/crates/l/kardia.svg)](LICENSE)

A Rust implementation of [Cordis](https://github.com/cordiverse/cordis), the plugin framework built on *spatiotemporal composability* ([paper](https://arxiv.org/abs/2608.25512)). Documentation: **https://kardia-dev.netlify.app**.

An app is a tree of **plugins**. Each plugin runs in a **fiber**:
- **Effects:** everything a plugin sets up (listeners, timers, services, child plugins) is tracked and reverted when the plugin stops.
- **Services:** plugins provide services and declare the services they depend on. A plugin starts once its dependencies are available, restarts when one is replaced, and stops when one goes away.
- **Live changes:** you can reconfigure, replace or hot-reload any plugin while the app runs, and the rest of the app follows.

## Contents

- [Requirements](#requirements)
- [Quick start](#quick-start)
- [The configuration file](#the-configuration-file)
- [Writing plugins in Rust](#writing-plugins-in-rust)
- [Hot reload and guest plugins](#hot-reload-and-guest-plugins)
- [Crates](#crates)
- [Development](#development)
- [Documentation](#documentation)
- [License](#license)

## Requirements

- Rust 1.85 or newer.
- Only needed to build WebAssembly guest plugins, or to run the full test suite: the `wasm32-unknown-unknown` target.

  ```sh
  rustup target add wasm32-unknown-unknown
  ```

## Quick start

The `kardia` binary loads an app from a YAML (or JSON) config file and keeps it in step with the file. From a clone of this repository:

```sh
cargo run -p kardia -- examples/kardia.yml
```

Or install the binary from crates.io and point it at your own config file:

```sh
cargo install kardia
kardia path/to/kardia.yml
```

While it runs, try these:
- **Change the config.** In `examples/kardia.yml`, set `disabled: false` on the `console` entry: the console logger starts. Set it back to `true` and it stops.
- **Hot-reload a plugin.** Edit the greeting in `examples/plugins/hello.wat`. The `hmr` entry reloads the WebAssembly plugin in place: the old instance logs its goodbye, then the new one logs your greeting.
- **Stop the app.** Press Ctrl-C. Shutting down does not write to the config file.

Without an argument, the binary reads `kardia.yml` from the working directory.

## The configuration file

The file is a list of **entries**. Each entry runs one plugin:

```yaml
- id: timer               # optional; generated (and written back) when missing
  name: timer             # a built-in plugin, or a path to a guest module
- id: plugins
  name: group             # a group runs the entries in its `config`
  group: true
  config:
    - id: console
      name: logger-console
      disabled: true      # true / false, or an expression
      config:
        levels: { default: 3 }   # 0 error … 3 debug
```

| Field | Meaning |
|---|---|
| `id` | Identifies the entry across edits. Missing ids are generated and written back to the file. |
| `name` | The plugin. Either a built-in name or a guest module path (see below). |
| `config` | The plugin's configuration. For a group, its list of entries. |
| `group` | Marks a group entry. |
| `disabled` | Stops the plugin without removing its entry. |
| `inject` | Extra services the entry depends on: a list of names, or a map of name → interception metadata. |
| `intercept` | Metadata for the services the plugin uses, keyed by service. For example, `logger: { name: db, level: 3 }`. |
| `isolate` | Gives services their own scope: `{ db: true }` makes `db` private to the entry, and `{ db: shared }` shares it between every entry using the label `shared`. |

**Expressions.** A value tagged `!!js` is an expression, such as `disabled: !!js "!settings.enabled"`.
- **Syntax:** a side-effect-free subset of JavaScript: literals, member access, arithmetic, comparisons, `&& || ??` and `?:`. Function calls are not supported.
- **Identifiers:** a name reads the JSON value of the service with that name. The entry must declare that service in `inject`.

**Edits go both ways.** Edits to the file are applied to the running app. Changes the app makes to its own entries are written back to the file.

### Built-in plugins

| Name | What it does | Config |
|---|---|---|
| `timer` | `timeout`, `interval`, `sleep`, `throttle` and `debounce`, each cancelled when its plugin stops | – |
| `logger-console` | Prints logs to the terminal | `levels` (name → `0`–`3`, plus `default`), `showTime`, `colors`, `maxLength`, `label` |
| `fs` | The file-watching service | – |
| `include` | Loads entries from another file as a subtree | `path`, `watch`, `initial`, `patches` |
| `group` | Runs the entries in its `config` | the entry list |
| `hmr` | Reloads guest modules when their files change | `base`, `watch: { enabled, files, debounce }` |

Each built-in also answers to its TypeScript package name, such as `@cordisjs/plugin-timer`.

The binary starts `fs` itself and includes the config file with `watch: true`.

## Writing plugins in Rust

Add the facade crate:

```toml
[dependencies]
kardia = "0.1"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Services are plain types. The `#[plugin]` macro reads a plugin's dependencies from its signature, so accessing a service it did not declare is a compile error:

```rust
use std::time::Duration;

use kardia::prelude::*;

#[derive(Service)]
#[service(crate = ::kardia)]
struct Database {
    url: String,
}

#[derive(Deserialize)]
struct DbConfig {
    url: String,
}

/// Provides `database`, and logs when it is stopped.
fn db() -> PluginDef {
    PluginDef::typed(|ctx: Context, config: std::sync::Arc<DbConfig>| async move {
        let logger = ctx.logger();
        ctx.on_dispose(move || logger.info("closing"))?;
        ctx.provide(Database { url: config.url.clone() })?;
        Ok(())
    })
    .name("db")
    .provides([Database::NAME])
}

/// Starts once `database` and `timer` are available, and stops with them.
#[plugin(crate = ::kardia)]
async fn greeter(ctx: Injected<(Database, Timer)>) -> Result<()> {
    let (db, timer) = ctx.deps()?;
    ctx.logger().info(format_args!("using {}", db.url));
    timer.interval(Duration::from_secs(1), || println!("tick"))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = Context::new();
    root.plugin::<TimerPlugin>(())?;
    root.plugin::<Greeter>(())?; // waits for `database`
    let db = root.plugin_def(&db(), json!({ "url": "postgres://primary" }))?;
    db.settled().await?;
    db.update(json!({ "url": "postgres://replica" }))?.await?; // greeter restarts with it
    db.dispose().await?; // greeter stops again
    Ok(())
}
```

A longer, runnable version, which also hot-replaces the database implementation:

```sh
cargo run -p kardia --example provider_consumer
```

To load your own plugins from a config file, register them in a `kardia::loader::Catalog` and pass it to `Loader::plugin`. `crates/kardia/src/main.rs` shows the full setup.

## Hot reload and guest plugins

Plugins compiled into the binary can be reconfigured live, but changing their code needs a restart.

**Guest plugins** are separate modules that `hmr` can reload in place. Any entry name that is not a built-in is a guest module path, relative to the config file. The `kardia` binary loads two kinds:
- **WebAssembly modules** (`.wasm`, or `.wat` text): run on wasmtime, sandboxed, one instance per plugin.
- **Dynamic libraries** (`.so` / `.dylib` / `.dll`): for development. They run in the host process without a sandbox, so a crash in the library brings down the app. Each version stays loaded until the process exits.

A third kind, **child processes**, is available to apps built on the library (`kardia::hmr::ProcessSource`), but not yet to the `kardia` binary, which picks a backend by file extension:
- Each plugin instance is its own process, speaking the guest interface as JSON lines over stdin and stdout.
- A crashing guest fails only its own calls.
- Reloading a module respawns its processes.
- Replace an executable by renaming a new file over it, since a running executable can't be written to.

To reload guest modules as they change, add an `hmr` entry with watch mode:

```yaml
- id: hmr
  name: hmr
  config:
    base: .                  # relative to this file; the default
    watch:
      enabled: true
      files: [plugins]       # patterns relative to base; `!` excludes
      debounce: 100          # ms; changes in one window reload together
- id: hello
  name: ./plugins/hello.wasm
```

### Writing a guest plugin in Rust

Create a `cdylib` crate that depends on `kardia-guest`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
kardia-guest = "0.1"
```

```rust
use kardia_guest::{Guest, Host, Level, Map, Meta, Request, Result, Value, json};

struct Counter {
    count: u64,
}

impl Guest for Counter {
    type Config = Value;

    fn meta() -> Meta {
        Meta::plugin("counter").provide("counter")
    }

    fn apply(host: &Host, config: Value, _imports: Map<String, Value>) -> Result<Self> {
        host.provide("counter", &json!({ "config": config }))?;
        host.log(Level::Info, "counting");
        Ok(Counter { count: 0 })
    }

    fn call(&mut self, _: &Host, request: Request) -> Result<Value> {
        match request.method.as_str() {
            "increment" => {
                self.count += 1;
                Ok(json!(self.count))
            }
            method => Err(format!("unknown method {method}").into()),
        }
    }
}

kardia_guest::export!(Counter);
```

The same source builds a WebAssembly or a dynamic-library guest:

```sh
cargo build --release --target wasm32-unknown-unknown   # target/wasm32-unknown-unknown/release/<crate>.wasm
cargo build --release                                   # target/release/lib<crate>.so (or .dylib / .dll)
```

For a process guest, add a binary to the crate (with `crate-type = ["cdylib", "rlib"]`, so it can use the library) whose `main` serves the guest:

```rust
fn main() {
    kardia_guest::serve::<my_guest::Counter>();
}
```

stdout carries the protocol, so a process guest must not print to it; it logs through `Host::log` or to stderr.

What a guest can do:
- **Use services.** `Host` can `log`, `provide` services, `view` the JSON value of services the guest declared with `Meta::inject`, and `invoke` services provided by other guests.
- **Be called from Rust.** Host-side code calls a guest's services through `kardia::hmr::GuestService` (`.value()` and `.call(method, args)`).
- **Fail safely.** An error or panic in a guest fails only that plugin or call. In a dylib or a process, the SDK catches the panic and returns it as an error. WebAssembly aborts on a panic, and the host reports the abort as a failed call. A process that exits fails its later calls with "the guest process exited".

Guests in other languages implement the JSON-based guest interface described in the `kardia-hmr` crate docs (`cargo doc -p kardia-hmr --open`). `examples/plugins/hello.wat` is a guest written by hand in WebAssembly text.

## Crates

| Crate | Contents |
|---|---|
| `kardia` | The facade (`kardia::prelude`) and the `kardia` binary |
| `kardia-core` | Contexts, effects, fibers, services, events, logger |
| `kardia-macros` | `#[derive(Service)]` and `#[plugin]` |
| `kardia-timer`, `kardia-logger-console`, `kardia-fs` | Built-in services |
| `kardia-loader` | Entries, groups, isolation, expressions, catalog |
| `kardia-include` | YAML/JSON config files, kept in sync both ways |
| `kardia-hmr` | Module graph, hot reload, watch mode, WASM, dylib and process backends |
| `kardia-guest` | SDK for guest plugins in Rust |

## Development

```sh
cargo test --workspace          # the WASM guest test needs the wasm32-unknown-unknown target
cargo clippy --workspace --all-targets
cargo fmt --all
cargo xtask arch                # lib.rs and mod.rs only declare and re-export modules
```

Some tests build fixture crates with cargo while they run, so the first run takes a little longer.

**Repository tasks** run as `cargo xtask <command>`:
- **`arch`:** checks the module structure.
- **`version <patch|minor|major>`:** bumps the workspace version everywhere it appears.
- **`tag [--push]`:** creates (and pushes) the release tag `v<version>`.

**CI** is split by what a change touches:
- **`ci.yml`:** the Rust checks, on a pinned Rust 1.96.0, since the compile-error snapshots of `kardia-macros` depend on the compiler version. It skips changes only to the docs site or Markdown.
- **`site.yml`:** checks and deploys the docs site.

**To release:**

```sh
cargo xtask version patch
git commit -am "chore(release): kardia 0.1.1"
git push
cargo xtask tag --push
```

The tag starts `release.yml`. It reuses CI's run of the tagged commit rather than testing it again, publishes every crate whose new version is not on crates.io yet, and creates a GitHub release. It can also be started by hand to resume a release that stopped part-way. [Contributing](https://kardia-dev.netlify.app/docs/development-contributing) has the details.

## Documentation

The documentation site, **https://kardia-dev.netlify.app**, is in Chinese and English. It has:
- **Guides** for every feature.
- **The paper's concepts.**
- **An analysis of the TypeScript Cordis code.**
- **The Rust design:** including, under [Implementation status](https://kardia-dev.netlify.app/docs/development-status), every place this implementation differs from the TypeScript one.

Its source is in [`site/`](site/). [Contributing](https://kardia-dev.netlify.app/docs/development-contributing) explains how to build it.

## License

kardia is licensed under the [GNU General Public License v3.0](LICENSE) (`GPL-3.0-only`).

It is a port of the TypeScript [Cordis](https://github.com/cordiverse/cordis), which is MIT-licensed. [`NOTICE`](NOTICE) keeps its copyright and license notice.
