# kardia

[English](README.md) | [简体中文](README.zh-CN.md)

[![CI](https://github.com/licyk/kardia/actions/workflows/ci.yml/badge.svg)](https://github.com/licyk/kardia/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/kardia.svg)](https://crates.io/crates/kardia)
[![docs.rs](https://img.shields.io/docsrs/kardia)](https://docs.rs/kardia)
[![License: GPL-3.0-only](https://img.shields.io/crates/l/kardia.svg)](LICENSE)

[Cordis](https://github.com/cordiverse/cordis) 的 Rust 实现。Cordis 是一个基于*时空可组合性*的插件框架（[论文](https://arxiv.org/abs/2608.25512)）。官方网站与文档：**https://kardia-dev.netlify.app**。

应用由一棵**插件**树组成，每个插件都在一个 **fiber** 中运行：

- **副作用（Effects）：** 插件创建的所有资源（监听器、定时器、服务、子插件）都会被追踪，并在插件停止时清理。
- **服务（Services）：** 插件提供服务，并声明自己依赖的服务。依赖就绪时插件启动，依赖被替换时插件重启，依赖消失时插件停止。
- **实时变更：** 应用运行期间，可以重新配置、替换或热重载任意插件，其余部分会随之调整。

## 目录

- [环境要求](#环境要求)
- [快速开始](#快速开始)
- [配置文件](#配置文件)
- [使用 Rust 编写插件](#使用-rust-编写插件)
- [热重载与客体插件](#热重载与客体插件)
- [TypeScript 与 Tauri](#typescript-与-tauri)
- [Crate 列表](#crate-列表)
- [开发](#开发)
- [文档](#文档)
- [许可证](#许可证)

## 环境要求

- Rust 1.85 或更新版本。
- 仅在构建 WebAssembly 客体插件或运行完整测试套件时，需要安装 `wasm32-unknown-unknown` 目标。

  ```sh
  rustup target add wasm32-unknown-unknown
  ```

## 快速开始

`kardia` 可执行程序从 YAML（或 JSON）配置文件加载应用，并使应用与文件保持同步。克隆仓库后运行：

```sh
cargo run -p kardia -- examples/kardia.yml
```

也可以从 crates.io 安装，然后指定自己的配置文件：

```sh
cargo install kardia
kardia path/to/kardia.yml
```

运行期间，可以尝试以下操作：

- **修改配置。** 在 `examples/kardia.yml` 中，将 `console` 条目的 `disabled` 设为 `false`，控制台日志插件就会启动；改回 `true` 后则会停止。
- **热重载插件。** 修改 `examples/plugins/hello.wat` 中的问候语。`hmr` 条目会就地重新加载 WebAssembly 插件：旧实例先输出告别日志，新实例再输出新的问候语。
- **停止应用。** 按 Ctrl-C。关闭应用不会写入配置文件。

未指定参数时，程序会读取工作目录中的 `kardia.yml`。

## 配置文件

配置文件是一个**条目**列表，每个条目运行一个插件：

```yaml
- id: timer               # 可选；缺失时自动生成并写回文件
  name: timer             # 内置插件名称，或客体模块路径
- id: plugins
  name: group             # 分组运行其 `config` 中的条目
  group: true
  config:
    - id: console
      name: logger-console
      disabled: true      # true / false，或表达式
      config:
        levels: { default: 3 }   # 0 为 error，…，3 为 debug
```

| 字段 | 含义 |
|---|---|
| `id` | 用于在配置变更前后识别同一条目。缺失时会自动生成并写回文件。 |
| `name` | 插件名称，可以是内置名称或客体模块路径（见下文）。 |
| `config` | 插件配置；对于分组，则是其条目列表。 |
| `group` | 标记分组条目。 |
| `disabled` | 停止插件，但保留其条目。 |
| `inject` | 条目额外依赖的服务，可以是名称列表，也可以是名称 → 拦截元数据的映射。 |
| `intercept` | 插件所用服务的元数据，以服务名为键。例如 `logger: { name: db, level: 3 }`。 |
| `isolate` | 为服务设置独立作用域：`{ db: true }` 使 `db` 仅供该条目使用，`{ db: shared }` 则让所有使用 `shared` 标签的条目共享该服务。 |

**表达式。** 带有 `!!js` 标签的值会被视为表达式，例如 `disabled: !!js "!settings.enabled"`。

- **语法：** JavaScript 的无副作用子集，支持字面量、成员访问、算术、比较、`&& || ??` 和 `?:`，不支持函数调用。
- **标识符：** 名称用于读取同名服务的 JSON 值。条目必须在 `inject` 中声明该服务。

**双向同步。** 文件中的修改会应用到正在运行的应用；应用对自身条目的修改也会写回文件。

### 内置插件

| 名称 | 功能 | 配置 |
|---|---|---|
| `timer` | 提供 `timeout`、`interval`、`sleep`、`throttle` 和 `debounce`，对应操作会在所属插件停止时取消 | – |
| `logger-console` | 向终端输出日志 | `levels`（名称 → `0`–`3`，以及 `default`）、`showTime`、`colors`、`maxLength`、`label` |
| `fs` | 文件监听服务 | – |
| `include` | 从另一个文件加载条目，作为子树运行 | `path`、`watch`、`initial`、`patches` |
| `group` | 运行其 `config` 中的条目 | 条目列表 |
| `hmr` | 客体模块文件变化时重新加载模块 | `base`、`watch: { enabled, files, debounce }` |

每个内置插件也支持使用对应的 TypeScript 包名，例如 `@cordisjs/plugin-timer`。

可执行程序会自行启动 `fs`，并以 `watch: true` 加载配置文件。

## 使用 Rust 编写插件

添加统一入口 crate：

```toml
[dependencies]
kardia = "0.1"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

服务是普通的 Rust 类型。`#[plugin]` 宏从插件的函数签名中读取依赖，因此访问未声明的服务会导致编译错误：

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

/// 提供 `database`，并在停止时记录日志。
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

/// 在 `database` 和 `timer` 就绪后启动，并随它们停止。
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
    root.plugin::<Greeter>(())?; // 等待 `database`
    let db = root.plugin_def(&db(), json!({ "url": "postgres://primary" }))?;
    db.settled().await?;
    db.update(json!({ "url": "postgres://replica" }))?.await?; // greeter 随之重启
    db.dispose().await?; // greeter 再次停止
    Ok(())
}
```

仓库还提供了更完整的可运行示例，其中也演示了数据库实现的热替换：

```sh
cargo run -p kardia --example provider_consumer
```

要从配置文件加载自己的插件，请将它们注册到 `kardia::loader::Catalog`，然后将其传给 `Loader::plugin`。完整配置流程见 `crates/kardia/src/main.rs`。

## 热重载与客体插件

编译进可执行程序的插件支持实时修改配置，但修改其代码后需要重启应用。

**客体插件（Guest plugins）** 是独立模块，`hmr` 可以就地重新加载它们。非内置插件的条目名称会被视为客体模块路径，相对于配置文件解析。`kardia` 可执行程序支持加载两类模块：

- **WebAssembly 模块**（`.wasm` 或 `.wat` 文本）：在 wasmtime 沙箱中运行，每个插件拥有独立实例。
- **动态库**（`.so` / `.dylib` / `.dll`）：用于开发。它们在宿主进程中运行，没有沙箱隔离，因此库崩溃会导致整个应用崩溃。每个版本都会保持加载状态，直到进程退出。

基于库构建的应用还可以使用第三种方式：**子进程**（`kardia::hmr::ProcessSource`）。`kardia` 可执行程序目前根据文件扩展名选择后端，尚不支持这种方式：

- 每个插件实例对应一个进程，通过 stdin 和 stdout 以 JSON Lines 格式通信，实现客体接口。
- 客体进程崩溃只会使其自身的调用失败。
- 重新加载模块时会重新启动对应进程。
- 更新可执行文件时，应将新文件重命名以替换旧文件，因为正在运行的可执行文件无法直接写入。

要在客体模块变化时重新加载它们，请添加启用监听模式的 `hmr` 条目：

```yaml
- id: hmr
  name: hmr
  config:
    base: .                  # 相对于当前文件；默认值为 .
    watch:
      enabled: true
      files: [plugins]       # 相对于 base 的匹配模式；`!` 表示排除
      debounce: 100          # 单位为毫秒；同一时间窗口内的变更合并重载
- id: hello
  name: ./plugins/hello.wasm
```

### 使用 Rust 编写客体插件

创建一个依赖 `kardia-guest` 的 `cdylib` crate：

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

同一份源码可以构建为 WebAssembly 或动态库客体插件：

```sh
cargo build --release --target wasm32-unknown-unknown   # target/wasm32-unknown-unknown/release/<crate>.wasm
cargo build --release                                   # target/release/lib<crate>.so（或 .dylib / .dll）
```

对于进程客体插件，在 crate 中添加一个二进制目标，并将 `crate-type` 设为 `["cdylib", "rlib"]`，以便该目标使用库。其 `main` 函数负责启动客体服务：

```rust
fn main() {
    kardia_guest::serve::<my_guest::Counter>();
}
```

stdout 用于传输协议数据，因此进程客体插件不能向其打印内容；日志应通过 `Host::log` 或 stderr 输出。

客体插件支持以下操作：

- **使用服务。** `Host` 可以通过 `log` 记录日志、通过 `provide` 提供服务、通过 `view` 读取使用 `Meta::inject` 声明的服务的 JSON 值，以及通过 `invoke` 调用其他客体插件提供的服务。
- **接受 Rust 调用。** 宿主代码通过 `kardia::hmr::GuestService` 调用客体插件的服务（`.value()` 和 `.call(method, args)`）。
- **处理失败。** 客体插件返回错误或发生 panic 时，仅对应插件或调用失败。在动态库或进程中，SDK 会捕获 panic 并将其作为错误返回。WebAssembly 会在 panic 时中止，宿主将该中止报告为调用失败。进程退出后，后续调用会返回错误 "the guest process exited"。

其他语言编写的客体插件需要实现基于 JSON 的客体接口，详见 `kardia-hmr` crate 文档（`cargo doc -p kardia-hmr --open`）。`examples/plugins/hello.wat` 是一个手写的 WebAssembly 文本格式客体插件。

## TypeScript 与 Tauri

当应用的一部分是 TypeScript 时（例如界面运行在 webview 中的 Tauri 应用），`@kardiajs` npm 包为这一侧提供同样的模型，并由桥把它连接到 Rust 应用：

| 包 | 内容 |
|---|---|
| [`@kardiajs/core`](js/packages/core/) | 上下文、fiber、可撤销副作用、响应式服务、事件和日志：`kardia-core` 的 TypeScript 移植，无任何依赖 |
| [`@kardiajs/timer`](js/packages/timer/) | 随所属插件结束的计时器 |
| [`@kardiajs/bridge`](js/packages/bridge/) | 桥的客户端：镜像 Rust 应用暴露的服务，使依赖它们的客户端插件恰在其暴露期间运行；双向异步调用；宿主事件 |
| [`@kardiajs/tauri`](js/packages/tauri/) | 桥在 Tauri 上的传输：一个命令加一个 `Channel` |
| [`@kardiajs/vue`](js/packages/vue/) | Vue 集成：扩展向 Vue 应用贡献组件、视图和路由，并随所属插件一同移除 |

桥的 Rust 一侧是 [`kardia-bridge`](crates/kardia-bridge/) crate。每个运行时各自保留自己的插件和副作用；桥在两者之间传递服务的可用性、调用和事件。[TypeScript 与 Tauri](https://kardia-dev.netlify.app/docs/guide-typescript) 介绍了两侧的用法。

## Crate 列表

| Crate | 内容 |
|---|---|
| [`kardia`](crates/kardia/) | 统一入口、`kardia::prelude`、内置插件目录和 `kardia` 可执行程序 |
| [`kardia-core`](crates/kardia-core/) | 上下文、可撤销副作用、fiber 生命周期、响应式服务解析、事件和日志 |
| [`kardia-macros`](crates/kardia-macros/) | 用于声明服务和插件依赖的 `#[derive(Service)]` 与 `#[plugin]` 宏 |
| [`kardia-timer`](crates/kardia-timer/) | 随调用方插件生命周期清理的超时、间隔、休眠、节流和防抖操作 |
| [`kardia-logger-console`](crates/kardia-logger-console/) | 控制台日志输出，支持格式化、颜色和日志级别配置 |
| [`kardia-fs`](crates/kardia-fs/) | 文件监听与过滤，订阅随调用方插件停止而清理 |
| [`kardia-loader`](crates/kardia-loader/) | 声明式插件树、条目、分组、服务隔离、表达式和插件解析 |
| [`kardia-include`](crates/kardia-include/) | YAML/JSON 条目树、文件监听、补丁及与运行时变更的双向同步 |
| [`kardia-hmr`](crates/kardia-hmr/) | 模块图、热替换、文件监听，以及 WebAssembly、动态库和子进程的客体服务桥接 |
| [`kardia-guest`](crates/kardia-guest/) | Rust 客体插件 SDK，提供宿主服务访问、模块导出和进程客体服务端 |
| [`kardia-bridge`](crates/kardia-bridge/) | 连接 TypeScript 运行时（如 Tauri webview）的桥的宿主端：暴露的服务、事件、客户端会话及其服务 |
| [`xtask`](xtask/) | 仓库内部工具：架构检查，以及 crate 与 npm 包的版本更新和发布标签管理；不发布 |

## 开发

```sh
cargo test --workspace          # WASM 客体测试需要 wasm32-unknown-unknown 目标
cargo clippy --workspace --all-targets
cargo fmt --all
cargo xtask arch                # lib.rs 和 mod.rs 仅声明和重新导出模块
```

部分测试会在运行时使用 cargo 构建测试用 crate，因此首次运行耗时稍长。

npm 包位于 Bun 工作区 [`js/`](js/) 中：

```sh
cd js
bun install
bun run check && bun run test   # 类型检查和单元测试
bun run test:e2e                # TypeScript 客户端对 Rust 宿主的端到端测试
bun run build
```

**仓库任务**通过 `cargo xtask <command>` 执行：

- **`arch`：** 检查模块结构。
- **`version <patch|minor|major>`：** 更新工作区版本号及其所有引用。
- **`tag [--push]`：** 创建（并推送）发布标签 `v<version>`。
- **`js-version <patch|minor|major>`** 与 **`js-tag [--push]`：** 对 npm 包执行同样的操作；npm 包有自己的版本和 `js-v<version>` 标签。

**CI** 按变更涉及的内容划分：

- **`ci.yml`：** 执行 Rust 检查，固定使用 Rust 1.99.1，因为 `kardia-macros` 的编译错误快照依赖编译器版本。仅修改文档站点、`js/` 或 Markdown 时会跳过。
- **`js.yml`：** npm 包的类型检查、测试和构建，以及桥的端到端测试。
- **`site.yml`：** 检查并部署文档站点。

**发布流程：**

```sh
cargo xtask version patch
git commit -am "chore(release): kardia 0.1.1"
git push
cargo xtask tag --push
```

标签会触发 `release.yml`。它复用该标签所指提交的 CI 结果，发布新版本尚未出现在 crates.io 上的各个 crate，并创建 GitHub Release。也可以手动触发，以继续中途停止的发布。详情见[贡献指南](https://kardia-dev.netlify.app/docs/development-contributing)。

npm 包以同样的方式发布，使用 `cargo xtask js-version` 和 `cargo xtask js-tag --push`：`js-v*` 标签会触发 `npm-release.yml`，它使用 npm 可信发布（GitHub 中不保存令牌）。[贡献指南](https://kardia-dev.netlify.app/docs/development-contributing#npm-releases)列出了 GitHub 和 npmjs.com 上的一次性设置。

## 文档

文档站点 **https://kardia-dev.netlify.app** 提供中文和英文内容，包括：

- **各项功能的使用指南。**
- **论文中的概念。**
- **TypeScript Cordis 源码分析。**
- **Rust 实现设计：** [实现状态](https://kardia-dev.netlify.app/docs/development-status)列出了本实现与 TypeScript 版本的各项差异。

文档源码位于 [`site/`](site/)，构建方法见[贡献指南](https://kardia-dev.netlify.app/docs/development-contributing)。

## 许可证

kardia 使用 [GNU General Public License v3.0](LICENSE)（`GPL-3.0-only`）许可证，客体插件 SDK [`kardia-guest`](crates/kardia-guest) 除外：它可任选 [MIT 许可证](crates/kardia-guest/LICENSE-MIT)或 [Apache License 2.0](crates/kardia-guest/LICENSE-APACHE)（`MIT OR Apache-2.0`），因此链接它的客体插件可以自行选择许可证。

本项目移植自采用 MIT 许可证的 TypeScript [Cordis](https://github.com/cordiverse/cordis)。[`NOTICE`](NOTICE) 保留了其版权与许可证声明。
