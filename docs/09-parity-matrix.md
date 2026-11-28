# 09 kardia 现有实现对照矩阵

本文把 `01`–`08` 的结论按「已具备 / 需修复 / 缺失 / 不适用」四态汇总，并给出代码位置与改动量估计。

「改动量」指实现 + 测试的净增量，按 S（<30 行或纯改名）、M（30–200 行）、L（200–800 行）、XL（>800 行或需新 crate/新模块）分档。

## 1. core

| 上游变更 | kardia 状态 | 位置 | 改动量 |
| --- | --- | --- | --- |
| `std:` 服务键 | 需改名 | `crates/*/src/*.rs` 的 `const NAME` 与全部 `Inject::new().with(...)`（19 处） | S |
| `Namespace<C, N>` 类型 | 不适用 | Rust 无属性代理 | — |
| `Fiber.Options.config` | 已具备 | `config_hooks`（`crates/kardia-core/src/state.rs:32`）、`resolve_config`（`lifecycle.rs:266`） | — |
| `Fiber.Options.commit` | 已具备 | `ConfigCommitted`（`events.rs:338`）、loader `on_committed`（`loader.rs:507`） | — |
| `Fiber.Options.dispose` | 已具备 | `PluginRemoved`（`events.rs:320`）、loader `on_removed`（`loader.rs:455`） | — |
| `Fiber.Options.getStack` | 不适用 | Rust 栈由 `std::backtrace` 提供 | — |
| `leak()` | **缺失** | `EffectHandle` 只有 `dispose()`（`effect.rs:325`），无解绑 | M |
| `buildStack` 改名 | 不适用 | — | — |
| 下划线成员私有化 | 已具备 | Rust 用 `pub(crate)` / 模块私有 | — |
| 关联键 `.` → `:` | 需确认 | kardia 无 `createTraceable` 机制；`Svc::meta()` 按 `Key` 直接匹配 | 需设计 |

## 2. loader

| 上游变更 | kardia 状态 | 位置 | 改动量 |
| --- | --- | --- | --- |
| entry 行为改 fiber 选项 | 已具备（等价物） | 见 core 三行 | — |
| `ctx[Entry.key]` | 已具备（等价物） | `entry_of_context()` | — |
| `Entry._hold` / transfer 归属 | **需修复** | `group_create()` 只改 `data.parent`（`group.rs:81`） | M |
| 自弃用 → `disabled: true` | 已具备 | `on_removed()`（`loader.rs:455`） | — |
| 依赖丢失清 verdict | 已具备 | `on_status()`（`loader.rs:534`） | — |
| `_enabled()` 考虑 UNLOADING 状态 | 已具备 | `is_disabled()`（`entry.rs:440`）+ `resolve_inject()` 重置 verdict | — |
| 组日志去重 | **缺失** | `show_log()` 无条件打 | S |
| `Entry.isOuter` | 已具备 | `OUTER_TAG` 标记 + `outer_def()` | — |

## 3. fs

| 上游变更 | kardia 状态 | 位置 | 改动量 |
| --- | --- | --- | --- |
| 包拆分 core/memory/node/conformance | 不跟 | `kardia-fs` 单 crate | — |
| `Path` 三态（字符串/名字数组/URL） | **缺失** | `FsExt::resolve(&str)` | L |
| `access` | **缺失** | — | S |
| `rename` | **缺失** | — | S |
| `remove` | **缺失** | — | S |
| `stat` / `exists` | **缺失** | — | M |
| `mkdir` | **缺失** | — | S |
| `symlink` / `readlink` / `realpath` | **缺失** | — | M |
| `copy` | **缺失** | — | M |
| `readdir` | **缺失** | — | M |
| `capabilities` 声明 | **缺失** | `FsProvider` 只有 `read()` | M |
| `FileRef::slice()` 区间读 | **缺失** | `file.rs` 只有 `stream()` | M |
| `FileRef::bytes/text/json/write` | **缺失** | — | M |
| `DirRef` | **缺失** | — | M |
| 原子写（临时文件 + rename + 退避） | **缺失** | include 自己拼（`include.rs` `_write_text`） | M |
| 独占写 `exclusive` | **缺失** | — | S |
| `ifMatch` 条件读写 | **缺失** | — | L |
| `std:fs` 键与拦截键 | 需改名 | `include.rs:200`、`serve.rs:28` | S |
| watch 结束丢弃 ready 回调 | 已具备（实现方式不同） | `watcher.rs:123-128` 的 `Signal::Ready` 先从 `hooks.ready` 取 listener，已移除则取不到 | — |
| 不监听 FIFO/socket/设备 | **缺失** | `watcher.rs` 无文件类型判断 | S |
| 内存 provider | **缺失** | — | L（若需要） |
| conformance 套件 | **缺失** | `kardia-fs/tests/read.rs` 单文件 | L（若需要） |

## 4. http

| 上游变更 | kardia 状态 | 位置 | 改动量 |
| --- | --- | --- | --- |
| `noProxy` | **缺失** | `config.rs` 只有 `proxy_agent` | M |
| event-stream 解码 | **缺失** | 无 SSE | M |
| 六方法大写归一 | 已具备 | `transport.rs:6` 校验即大写；`http::Method` 常量本身已归一 | — |
| `std:http` 键 | 需改名 | `service.rs:13` | S |
| `decoder()` 注册表 | 不跟（Rust 用枚举） | `decode.rs` | — |
| `data:` / `file:` URL | 缺失（已知） | — | 不在本轮范围 |
| SOCKS | 不跟 | — | — |

## 5. server

| 上游变更 | kardia 状态 | 位置 | 改动量 |
| --- | --- | --- | --- |
| 查询串含 `?` | **需修复** | `request.rs:29-32` | S |
| 表单体只编码一次 | **需修复** | `response.rs:141-150` `or_insert` | S |
| IPv6 主机括号 | **需修复** | `server.rs:120-135` | S |
| 畸形百分号 → 400 | **需修复** | `pattern.rs:96-108` 抛 Err → 500 | M |
| 畸形百分号 → 路由不匹配 | **需修复** | 同上，`return` 而非 `throw` | M |
| 路由参数跨编码边界不匹配 | **需修复** | 同上 | 含上一行 |
| static 目录先于扩展名 | **需修复** | `serve.rs:91-110` vs `:52` | M |
| `std:server` 键 | 需改名 | `server.rs:30,47` | S |
| proxy `target` 前缀语义 | **需重写** | `proxy/plugin.rs:50-52` | L |
| proxy `isolateOrigin` | **缺失** | — | M |
| proxy `requestHeaders` 映射 | **缺失** | — | M |
| proxy hop-by-hop + `Connection` 展开 | **缺失** | — | S |
| proxy 双重越界比较 | 思路不同 | 现有 `SEGMENT` + `locate` | 含在重写内 |
| 删除 `ServerProxy` 服务 | **保留**（破坏下游） | `proxy/plugin.rs:37-43` | 决策项 |
| acl `std:server` | 需改名 | `acl/plugin.rs:26` | S |
| echo `std:server` | 需改名 | `echo/plugin.rs:12` | S |
| 请求/响应 tracker 关联键 | 不适用 | Rust 无 tracker | — |

## 6. timer

| 上游变更 | kardia 状态 | 位置 | 改动量 |
| --- | --- | --- | --- |
| 节流用单调时钟 | 已具备 | `service.rs:167` `tokio::time::Instant` | — |
| 重入时保留尾部句柄 | **需修复** | `schedule.rs:44` `state.timer = timer`（无条件覆盖） | S |
| `std:timer` 键 + mixin 名 | 需改名 | `service.rs:27` | S |

## 7. env / hmr / include

| 上游变更 | kardia 状态 | 位置 | 改动量 |
| --- | --- | --- | --- |
| 空 env 覆盖文件 | 无 env 插件 | — | XL（若新增） |
| hmr 符号链接真实路径 | **需修复** | `hmr.rs:462` `normalize()` 只做 `..` 折叠，无 `canonicalize` | M |
| hmr Node 22 钩子 | 不适用 | — | — |
| hmr `std:hmr` + `std:fs` 注入 | 需改名 | `hmr.rs:37,98` | S |
| include 悬空别名 | 已具备位置信息 | `yaml.rs:187-191` | S（仅细化消息） |
| include `std:fs` 拦截 | 需改名 | `include.rs:200` | S |

## 8. 工具链

| 上游变更 | kardia 状态 | 结论 |
| --- | --- | --- |
| 版本 bump | 独立版本线 | 不跟 |
| yakumo | 未使用 | 不跟 |
| 发布流程改「PR 合并触发」 | 自有流程 | 选择性借鉴 |
| Node 22 CI 矩阵 | 测 Node 24 | 视下游需求 |
| 文档语言统一英文 | 中英双语 | 保持双语 |

## 9. 汇总

按状态统计：

| 状态 | 数量 |
| --- | --- |
| 已具备（含等价实现） | 16 |
| 需改名（`std:`） | 11 |
| 需修复（缺陷） | 9 |
| 缺失（需新增） | 21 |
| 不适用 | 8 |
| 决策项 | 1（`ServerProxy` 去留） |

按改动量统计（不含「不适用」与「已具备」）：

| 档次 | 数量 | 主要项 |
| --- | --- | --- |
| S | 16 | `std:` 改名、timer 重入、server 三处小修、proxy hop-by-hop、FIFO 跳过 |
| M | 13 | `leak()`、transfer、`noProxy`、SSE、static 顺序、proxy 头映射 |
| L | 7 | `Path` 三态、条件读写、proxy 重写、内存 provider、conformance |
| XL | 1 | env 插件（本轮不做） |

## 10. 关于 `std:` 改名的规模

`std:` 是「看起来最小、实际最琐碎」的一项。需要改动的位置：

| 类别 | 数量 | 说明 |
| --- | --- | --- |
| `Service::NAME` 常量 | 11 | `core`、`service.rs` 各服务 |
| `Inject::new().with(...)` | 19 | 含 4 处测试 |
| `ctx.intercept("fs", ...)` | 2 | include、server-static |
| `ctx.get::<T>()` 依赖注入 | 不经字符串 | 不受影响 |
| 配置文件与示例 | ~6 | `examples/`、README |
| 文档与站点的服务名 | 多处 | `site/content/**` |

因此建议**集中一次性做**，不要和其它语义变更混在一个提交里，否则 review 无法分辨。

## 核对命令

```bash
cd /root/code_workspace/kardia
grep -rn "const NAME: &'static str" crates/*/src/*.rs
grep -rn 'Inject::new().with(' crates/*/src/*.rs
grep -rn 'intercept("' crates/*/src/*.rs
```
