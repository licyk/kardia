# 12 执行记录

本文记录 `11-implementation-plan.md` 的实际落地结果，包括与计划的偏差及其原因。计划本身保留不改，便于对照。

## 1. 已完成

| 计划项 | 提交 | 实际结果 |
| --- | --- | --- |
| 基线更新（pin、inventory、适配层） | `6b3c9af` | 完成，含 `it.each` 的 `as const`/`%j`/行式用例适配 |
| P0-1 查询串含 `?` | `2959524` | 完成 |
| P0-2 表单 content-type 与 boundary | `2959524` | 完成，用 `form_type` 精确识别「这个头是我写的」 |
| P0-3 IPv6 主机括号 | `2959524` | 完成，覆盖 80/443/其他三个分支 |
| P0-4 timer 重入保留尾部句柄 | `2959524` | 完成 |
| P1-1 `leak()` | `58cd9c0` | 只加了原语，**未加 loader 持有者机制**，理由见第 3 节 |
| P1-3 畸形百分号 400 | `82ee9f8` | 完成，同时让路由捕获失败转为「不匹配」 |
| P2-2 static 目录先于扩展名 | `3c2d860` | 完成 |
| P2-3 http `noProxy` | `2896418` | 完成 |
| P3-1 事件带 `FiberId` | `eaedf32` | 完成 |
| P3-2/P3-3 bridge 增强 | `8ff4d17` | 完成 |
| P2-4 proxy 前缀化 + 头映射 + `isolateOrigin` | `4fe3e11` | 完成，含删除 `ServerProxy` |
| P2-1 fs `Path` 三态 + entry 解析 | `bb51304` | 完成 |
| P1-2 `std:` 服务键 | 本次 | 完成，显式改名（非运行时归一） |
| fs entry 操作（`stat`/`write`/`rename`/`remove`）+ `include` 改造 | 本次 | 完成，见第 3.3 节 |
| watcher 跳过 FIFO/socket/设备 | 本次 | 完成 |
| include 别名报错区分 | 本次 | 完成 |
| http SSE（`event-stream`） | 本次 | 完成 |

## 2. 验证状态

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo xtask arch` | 通过 |
| `cargo test --workspace` | 通过，无失败目标 |
| `cargo clippy -p kardia-loader --no-default-features` | 通过 |
| `cargo test -p kardia-loader --no-default-features` | 通过 |
| `cargo clippy -p kardia-hmr --no-default-features --features process` | 通过 |
| `bun run check` / `bun run test` | 通过，155 个测试 |
| `bun run server:inventory --check` | 通过，156 个用例 |
| `bun run test:server:reference` | **224 / 224 通过** |

## 3. 与计划的偏差

### 3.1 P1-1 没有引入 loader 持有者机制

计划要求照搬上游 `Entry._hold` / `_release`，并新增 `EffectHandle::leak()` 支撑它。

实际验证：kardia 的 entry context 在转移时通过 `patch_fibers()` 调用 `data.ctx.reparent(&parent)` 重建 ctx 链，因此 group 卸载不会连带 entry。写了一个「A 组启动 → 转移到 B 组 → 卸载 A 组 → B 组插件仍在运行」的测试，**在没有任何 loader 改动的情况下通过**。

结论：上游 `8dbd5d3` 修的是 Cordis 的 context 原型链模型特有的缺陷，kardia 的 reparent 模型不存在它。因此只保留 `EffectHandle::leak()`（与上游 API 对齐、可独立使用），不引入无消费者的持有者簿记——符合仓库规则「不为臆测的问题增加复杂度」。

### 3.2 P1-2 用显式改名，未用运行时归一

计划写的是「入口单向归一化」，让 `fs` 与 `std:fs` 两种写法都能用。实际尝试后放弃：

- `Key`、`Inject::contains`/`meta`、`Scope::intercepts`、`State::realm_of` 四处都要归一才自洽，漏一处就是「依赖永远等不到」的静默挂起。
- 归一化后 `Plugin::NAME` 与 `Service::NAME` 会撞车（两者共用同一个常量语义），导致插件名变成 `std:server`、catalog 查不到。

改为与上游一致的**显式改名**：只改 `Service::NAME`，`Plugin::NAME` 保持原样；所有 `inject`、`intercept`、`isolate` 与示例配置同步。这样语义是确定的，失败会立刻暴露为「服务查不到」而不是挂起。

代价是不再接受 `inject: ["fs"]` 的旧写法。这是刻意的破坏性变更，与上游一致。

### 3.3 fs entry 操作在第二轮补上，并暴露了一个真实缺陷

第一轮只做了 `Path` 三态语义，理由写的是「没有消费者」。这个理由是错的：`include` 一直在写文件，只是直接用 `std::fs`，绕过了 `kardia-fs`。它因此无法通过 provider 工作（非 `file:` scheme 一律失败），也不参与 fs 的拦截与生命周期。

第二轮补上 `Capabilities` 与 `stat`/`write`/`rename`/`remove`，并让 `include` 改走 provider。改造中 `sync` 的两个 dispose 测试立刻失败，原因是：**写盘发生在插件卸载期间，而 `file:` 的 mount 由另一个插件持有，此时已被卸载**。

修复方式：`include` 在启动时用 `Fs::provider_of()` 捕获 provider 句柄（`Provider`），此后读写都走这个句柄。`Provider` 持有 `Arc<Mount>`，因此 provider 的生命周期跟随 `include`，与 mount 列表的增删无关。

这个缺陷只会在「卸载时写盘」这条路径上暴露，而它恰好是两个遗留测试覆盖的。

## 4. 剩余 parity 失败

无。`bun run test:server:reference` 在 pin 到 Cordis `6ad12cf` 后 **224 / 224 通过**，与基线 `36dc7fb` 时的通过数一致，且对照的是新的上游语义。

期间还修了适配层的一处遗漏：`std:` 迁移时 `.http(...)` 没有改成 `.std.http(...)`，导致 26 个 client 对照全部报告「上游出错」。这是适配层 bug，不是 kardia 实现差异。

## 5. 仍未跟进的上游项

| 项 | 状态 | 说明 |
| --- | --- | --- |
| fs `realpath`/`copy`/`mkdir`/`readdir`/`symlink` | **已完成** | 见第 5.3 节 |
| fs 区间读、原子写、`ifMatch`、`DirRef` | **已完成** | 见第 5.3 节 |
| fs 内存 provider 与 conformance 套件 | **已完成** | 新增 `kardia-fs-memory` 与 `kardia-fs-conformance` |
| http `data:` URL | **已完成** | 见第 5.4 节 |
| http `file:` URL | 未做 | 依赖方向不允许；见第 5.4 节的理由与替代方案 |
| 上游 `ctx.std.x` 语法糖 | 无法跟进 | Rust 无属性代理；键名已对齐 |
| 上游 `getStack` 栈帧拼接 | 无法跟进 | Rust 用 `std::backtrace` |
| 上游发布流程（PR 合并触发） | 未做 | kardia 有自己的发布流程 |

## 5.1 经核实不存在的上游项

分析时列为「缺失」，第二轮核实后确认 kardia 本就没有该问题，不实现：

| 项 | 核实结果 |
| --- | --- |
| 组日志去重 | `show_log` 已经跳过 `group`，且 kardia 没有 `reload` 日志动作，不存在重复 |

## 5.2 第二轮补上的项

| 项 | 内容 |
| --- | --- |
| fs `Capabilities` 与四个 entry 操作 | provider 未声明的操作在调用前报 `NotSupported` |
| `include` 改走 fs | 读写重命名删除全部经由捕获的 `Provider`，不再直接调用 `std::fs` |
| `Faulty` provider | 测试可在真实 provider 上注入 rename 失败，替代替换 `std::fs::rename` 的闭包 |
| watcher 跳过特殊文件 | FIFO、socket、块/字符设备返回 `NotSupported`，避免平台 watcher 阻塞 |
| include 别名报错 | 区分「锚点在后」（`must follow its anchor`）与「不存在」（`unresolved alias`） |
| http SSE | `EventStream` 增量解析器，处理跨 chunk 的 CRLF 与 UTF-8 分裂 |

## 5.3 第三轮：fs 能力补齐

| 项 | 内容 |
| --- | --- |
| `Capabilities` | 新增 `mkdir`/`copy`/`copy_recursive`/`realpath`/`symlink`/`readdir`/`links`/`read_range`/`atomic_write` |
| 区间读 | `FsProvider::read_range` 接收 `ReadOptions{start,end,if_match}`；`FileRef::slice()` 按 `Blob.slice()` 语义解析，负数从末尾算、越界截断，每次读取时解析 |
| 条件读写 | `Stats.version` 与 `WriteOptions.if_match`，不符时 `VersionMismatch` |
| 原子写 | `LocalFs` 用同目录临时文件加 rename；内存 provider 天然原子 |
| `RemoveOptions` | `recursive` 与 `force`，`Provider::remove_with` 转发 |
| entry 操作 | `mkdir`/`copy`（含 `dereference`）/`realpath`/`symlink`/`readlink`/`readdir` |
| `DirRef` | `dir()`/`file()`/`list()`，`DirEntryRef` 携带列出时的类型 |
| `kardia-fs-memory` | 按 URL 路径为键的内存树，逐文件版本号，支持空前缀（服务所有未被其它 mount 认领的位置） |
| `kardia-fs-conformance` | 11 条规则，`Suite::boot` 与 `Suite::boot_at` 让任意 provider 都能跑；内存与本地 provider 都通过 |

期间修复的三个真实缺陷：

1. **`LocalFs` 的版本粒度不足**。原用 mtime 纳秒值，同一时钟精度内连续两次写入得到相同版本，`if_match` 因此检测不到变化。改为写后 `touch()` 单调递增地设置 mtime。
2. **空 mount 前缀不可用**。`normalize_prefix("")` 直接 `Url::parse("")` 失败，`fs.mount("")` 无法表达「服务所有位置」。改为返回空前缀。
3. **`remove` 不支持递归**。目录非空时一律 `DirectoryNotEmpty`，无法清理整棵子树。新增 `RemoveOptions`。

## 5.4 第四轮：`data:` URL

`Http` 现在直接应答 `data:`，不经任何传输：

| 形态 | 结果 |
| --- | --- |
| `data:,hello` | `200`，`text/plain;charset=US-ASCII` |
| `data:application/json,%7B%7D` | `200`，`application/json`，内容 `{}` |
| `data:...;base64,AAEC` | `200`，解码为字节 |
| 非 GET 方法 | `405 Method Not Allowed`，与上游一致 |
| 缺少逗号 | 请求失败 |

实现细节：逗号只切分一次，`data` 里的逗号保留；base64 忽略填充与换行；非 base64 部分按 UTF-8 百分号解码。7 个单元测试与 1 个经由真实 `request()` 的集成测试。

### 为什么 `file:` 仍然不做

上游用 `@cordisjs/fetch-file` 直接读操作系统文件。kardia 不能照做，因为那正是我在 `include` 上刚修的错：绕过 `std:fs` 会让拦截与 mount 失效，非 `file:` 的 provider 一律不可用。

正确的实现必须经由 `std:fs`，但 `kardia-http` 不依赖 `kardia-fs`，反过来 `kardia-fs` 也不应依赖 `kardia-http`。打通需要新增一个 scheme handler 注册表（`kardia-http` 定义 trait，宿主注入 `std:fs` 的实现）。这层抽象目前只有一个使用者，且 `std:fs` 本身已经能直接读文件——需要这个能力的地方应当调用 `ctx.std.fs.file(url).text()`，而不是绕道 http。

结论：不做 `file:`，替代方案是直接使用 `std:fs`。若将来出现「同一段代码既要读 http 又要读 file」的真实需求，再引入 scheme handler，届时它不是推测性抽象。

## 5.5 JS 侧跟进：不需要新增包

问：Rust 侧这些新接口，JS 侧是否需要新增包来公开？

**结论：不需要新增包。** `@kardiajs/*` 是 Cordis 的 **TypeScript 移植**（独立实现，不是 Rust 的绑定层），Rust crate 的 API 不会自动映射过去。逐项核对：

| Rust 新增 | JS 侧对应物 | 结论 |
| --- | --- | --- |
| `EffectHandle::leak()` | 无（JS 的 `DisposableList` 本来就可 `delete`） | 无需暴露 |
| `Panicked` 带 `FiberId` | JS 的 `Event` 没有 `Panicked` 类型，失败直接抛 `AggregateError` | 无对应物 |
| `emit_scoped` / `handle_as` | 见下 | **需要跟进，但已在 `@kardiajs/bridge` 内** |
| fs 的区间读/条件写/`DirRef`/内存 provider | JS 侧无 fs 服务 | 不适用 |
| `data:` URL | JS 侧无 http 服务 | 不适用 |
| SSE 解析器 | JS 有原生 `EventSource` | 不适用 |

唯一需要跟进的是 **`handle_as`**：宿主可以自己命名客户端，但 JS 客户端过去只知道自己上报的 `client`。若宿主覆盖了它，客户端的重连会以旧名再开一个会话，而不是替换掉自己的上一个——同一窗口因此会有两个会话。

这一轮补的就是这个：

- `open` 回执增加 `client`，报告**实际生效**的身份（`crates/kardia-bridge`）
- `@kardiajs/bridge` 读取该字段并在被覆盖时采用它，重连因此仍能替换自己的会话
- 新增 1 个测试：请求带 `main`、宿主答 `real`、断线重连后请求改为带 `real`

版本变化：`@kardiajs/bridge` 的公开类型 `OpenReply` 多了一个必填字段，属于破坏性变更，应在下次 `cargo xtask js-version minor` 时体现。

### 为什么 JS 侧没有 `fs`、`http`、`server` 包

这不是「上游没有」，需要说清楚：**上游 Cordis 的 JS 侧有** `packages/fs`、`packages/http`、`packages/server`（含 acl/proxy/static/echo）。kardia 只移植了 5 个包，原因是定位不同：

**kardia 的 JS 侧是应用内部的一侧，不是独立服务器。** 按站点 `guide-typescript` 的表述，`@kardiajs` 服务的是「应用的一部分是 TypeScript」的场景——Tauri webview 里的界面、Vue 组件与扩展。宿主本身是 Rust 应用，`std:fs`、`std:http`、`std:server` 都在 Rust 侧，JS 通过桥访问它们暴露的服务。

上游那三个包实现的是 **Node 运行时能力**，且确实依赖 Node：

| 包 | 依赖 | 能否在浏览器/webview 运行 |
| --- | --- | --- |
| `fs/core` | `node:url`（`pathToFileURL`）、`process.cwd()` | 否 |
| `http` | `node:module`（`createRequire`）、`undici` | 否 |
| `server` | Node `http`/`ws` | 否 |

而 `@kardiajs/core` 的自我声明是「no dependencies; runs in browsers and Node」。要打包进 webview，就不能引入这三个。

**这不是遗漏，是有替代路径：**

- 读文件 → 桥调用 Rust 的 `std:fs`，或 webview 自己的 `fetch`
- HTTP 请求 → 浏览器原生 `fetch`
- 事件流 → 浏览器原生 `EventSource`（这也是 SSE 不需要 JS 包的原因）
- HTTP 服务 → Rust 侧 `std:server`

**什么情况下需要补？** 如果要把 `@kardiajs/core` 当作 **Node 服务端框架**使用（脱离 Rust 宿主、独立跑 Express 式服务），那三个包才有意义。当前仓库没有这个用法。

## 6. 下游反馈状态

| 编号 | 状态 |
| --- | --- |
| K1 事件带 `FiberId` | **已实现**（`eaedf32`） |
| K2 `emit_scoped` | **已实现**（`8ff4d17`） |
| K3 调用上限 | kardia 已有 `BridgeOptions.timeout`；并发上限待下游澄清 |
| K4 宿主注入 client 标识 | **已实现**（`8ff4d17`） |
| K5 guest `invoke` 指向宿主服务 | 未做 |
| K6 单实例并发 export | 未做（协议级） |
| K7 host→guest `event` | 未做（依赖 K6） |
| K8 export 外主动调用 | 未做（依赖 K6） |
| K9 重启/退避 | 0.3.0 已具备；缺熔断与指数退避 |

## 6. 复核命令

```bash
cd /root/code_workspace/kardia
git log --oneline 6b3c9af..HEAD
cargo test --workspace
cd js && bun run test:server:reference
```
