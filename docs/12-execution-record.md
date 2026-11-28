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
| http `data:`/`file:` URL | 未做 | parity README 已声明不追求完整 decoder 生态；Rust 侧应走 `std:fs` |
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
