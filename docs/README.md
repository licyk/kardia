# Cordis 跟进分析文档索引

这套文档记录一次针对上游 Cordis 的跟进分析，以及由此得到的 kardia 实现计划。

## 基线与范围

| 项目 | 值 |
| --- | --- |
| 移植基线提交 | Cordis `36dc7fb5e271143039e6b8b6df59840fc0886c1b`（`feat(fs)!: treat base URLs as directories and drop base options` #346，2026-11-15） |
| 跟进目标 | Cordis `6ad12cf2b39e7a22fe54b2f4568e948ecc778e97`（`ci: publish package releases without releasing cordis` #392，2026-11-27） |
| 提交数 | 37 |
| 变更规模 | 146 个文件，+10441 / −3457 |
| 基线记录位置 | `js/tests/server-parity/upstream.ts` 的 `UPSTREAM_SHA`，以及 `js/tests/server-parity/README.md` |
| kardia 版本 | `0.3.0`（`Cargo.toml`），`@kardiajs/*` `0.2.0` |

基线提交是 `origin/main` 的祖先，因此本次跟进是**线性前进 37 个提交**，没有分叉合并。

## 文档

按阅读顺序：

| # | 文档 | 内容 |
| --- | --- | --- |
| 01 | [`01-upstream-overview.md`](01-upstream-overview.md) | 37 个提交的分类、每个包的改动量、破坏性变更清单、与 kardia 的总对照 |
| 02 | [`02-core-context-fiber.md`](02-core-context-fiber.md) | `std` 命名空间、`Fiber.Options`、`leak()`、下划线成员私有化、`buildStack` 改名 |
| 03 | [`03-loader-entry.md`](03-loader-entry.md) | entry 行为下移到 fiber 选项、自弃用判定、entry 跨组转移的持有者、group 日志归属 |
| 04 | [`04-fs-service.md`](04-fs-service.md) | 包拆分（core/memory/node/conformance）、entry 操作、引用对象、区间读、原子写、目录基准语义 |
| 05 | [`05-http.md`](05-http.md) | `noProxy`、event-stream 解码、方法大写归一 |
| 06 | [`06-server.md`](06-server.md) | `std:server`、畸形百分号路径 400、查询串含 `?`、IPv6 主机括号、表单体只编码一次、Static 目录重定向顺序、Proxy 前缀化重构、ACL |
| 07 | [`07-timer-env-hmr-include.md`](07-timer-env-hmr-include.md) | 单调时钟节流、重入时保留尾部句柄、空环境变量覆盖、符号链接真实路径、同步模块钩子、YAML 悬空别名 |
| 08 | [`08-toolchain-release.md`](08-toolchain-release.md) | 版本号、yakumo 升级、发布流程改为 PR 合并触发、构建矩阵 |
| 09 | [`09-parity-matrix.md`](09-parity-matrix.md) | kardia 现有实现逐项对照矩阵（已具备 / 部分具备 / 缺失），含代码位置 |
| 10 | [`10-downstream-feedback.md`](10-downstream-feedback.md) | 下游 `Hanafubuki-dev/KARDIA-ADOPTION-ANALYSIS.md` 的 K1–K9 诉求逐条评估 |
| 11 | [`11-implementation-plan.md`](11-implementation-plan.md) | 汇总所有文档后的完整实现计划：阶段、任务、验收、风险 |
| 12 | [`12-execution-record.md`](12-execution-record.md) | 计划的实际落地结果、与计划的偏差及原因、剩余 parity 失败的性质 |

## 结论摘要

1. **上游这 37 个提交对本项目影响最集中的三块**是：`std` 命名空间（触及每一个官方服务）、fs 的能力扩张（几乎等于重写）、loader entry 行为的归属（从事件监听搬到 fiber 选项）。
2. **必须跟进的破坏性变更**有五处：`std:` 前缀、`Fiber.Options` 取代 `getOuterStack` 回调、`ctx.entry` 移到 `ctx[Entry.key]`、fs 包路径与配置项改名、server-proxy 配置从「目录」改成「前缀」。
3. **上游本轮没有引入**任何与下游 K1–K9 诉求直接对应的能力：事件仍不带 `FiberId`、bridge 仍信任客户端、guest 协议未变。因此下游九条诉求需要本项目自己实现，不能指望上游。
4. 计划按四条主线推进：核心与加载器对齐、HTTP/Server 语义对齐、fs 能力补齐、下游反馈落地。
5. **下游九条诉求里两条的原始表述过时**：K3 的「调用上限」kardia 已有时长上限，补了并发上限 `max_in_flight`；K9 的重启/固定退避已在 0.3.0 具备，补了 `maxRetries` 与指数退避。
6. **九个上游修复在 kardia 上同样存在**（分析时的状态）：查询串含 `?`、表单体 content-type、IPv6 主机括号、timer 重入句柄、static 目录顺序、proxy hop-by-hop、FIFO 跳过、entry 转移归属、`Fiber::leak` 缺失。前六项已修，第七项（FIFO 跳过）已做，第八项经实测在 kardia 不存在，第九项已补原语。详见 `12`。
7. **下游 K1–K9 全部落地**。K6–K8 是协议级改动，引入 `kardia-guest/3`：单实例 `call` 并发、host→guest `event`、guest 脱离 export 调 host（`Host::detached()`）。唯一的 SDK API 破坏是 `Guest::call` 从 `&mut self` 改为 `&self`。旧 guest（kardia-guest 0.2.x）在新 host 上照常运行；新 guest 不能在旧 host 上运行。详见 `10` 的 K6–K8 与 `12` 第 5.6 节。

## 已落地

见 [`12-execution-record.md`](12-execution-record.md)。计划内的 P0、P1、P2、P3 全部完成：

- 基线 pin 更新到 `6ad12cf`，inventory 脚本支持 `as const`/`%j`/行式用例
- P0 四项缺陷：查询串、表单 boundary、IPv6 括号、timer 重入
- `EffectHandle::leak()`、畸形百分号 400、static 目录顺序、`noProxy`
- proxy 前缀化 + 头映射 + `isolateOrigin`，并删除 `ServerProxy`
- fs `Path` 三态语义（字面名、网络路径、盘符、cwd 基准）
- `std:` 服务键（显式改名，与上游一致）
- 下游 K1、K2、K4

- fs entry 操作（`stat`/`write`/`rename`/`remove` + `Capabilities`），`include` 改走 provider
- watcher 跳过 FIFO/socket/设备、include 别名报错区分、http SSE
- `data:` URL；`@kardiajs/bridge` 采纳 `OpenReply.client`
- 下游 K1–K9 全部：`fiber_of`、`handle_with`/`owner`、`max_in_flight`、`HostAction`、`kardia-guest/3`（K6/K7/K8）、`maxRetries` + 指数退避

**`bun run test:server:reference` 为 224 / 224 通过。**

fs 已补齐区间读、原子写、`ifMatch`、`DirRef`、`realpath`/`copy`/`mkdir`/`readdir`/`symlink`，并新增 `kardia-fs-memory` 与 `kardia-fs-conformance` 两个 crate。

http 的 `data:` URL 已支持；`file:` 交由 `std:fs` 处理。

`pykardia` 的 host 侧同步支持 `kardia-guest/2` 与 `kardia-guest/3`；其 guest SDK 只说 v3。`tests/parity/rust_guest/` 的 0.2.2 固定副本未动，继续作为 v2 验收。

**没有未跟进的项。**

## 核对方式

所有结论都以代码为准，命令见各文档末尾的「核对命令」一节。基线中的 `cordis/` 目录被 `.gitignore` 忽略，由 `js/scripts/prepare-server-reference.ts` 按 `UPSTREAM_SHA` 重新检出。
