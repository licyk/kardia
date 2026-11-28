# 01 上游变更总览

范围：Cordis `36dc7fb` → `6ad12cf`，37 个提交。

## 1. 按包的改动量

按首层包聚合（`packages/fs/*` 合并为 `fs`，`packages/server/*` 各算一个）：

| 包 | 文件 | + | − | 性质 |
| --- | --- | --- | --- | --- |
| `fs`（core + memory + node + conformance） | 31 | 7055 | 884 | 大规模功能扩张 + 包拆分 |
| `server/proxy` | 3 | 418 | 132 | 破坏性重构 |
| `include` | 13 | 415 | 590 | 测试重写 + 别名修复 |
| `http` | 5 | 391 | 31 | 新特性 |
| `core` | 15 | 389 | 72 | 破坏性重构 + 新 API |
| `loader` | 13 | 343 | 223 | 破坏性重构 |
| `server/core` | 4 | 297 | 97 | 修复 + `std` 迁移 |
| `cli` | 6 | 214 | 214 | 仅 `std` 改名 |
| `ci`（`.github`） | 3 | 124 | 77 | 发布流程重写 |
| `hmr` | 9 | 146 | 112 | 修复 + `std` 迁移 |
| `fs-node`（旧路径 → `fs/node`） | 9 | 129 | 605 | 移动 + 修复 |
| `server/static` | 11 | 90 | 64 | 修复 + `std` 迁移 |
| `env` | 3 | 86 | 87 | 修复 + `std` 迁移 |
| `timer` | 3 | 60 | 15 | 两个缺陷修复 |
| `server/acl` | 3 | 59 | 59 | 仅 `std` 迁移 + 测试 |
| 其他（`server/echo`、`logger-console`、`cli-cordis`、`http-socks`、`package-manager`） | 11 | 59 | 58 | 仅 `std` 迁移 |
| 根目录（`tsconfig`、`yakumo.yml`、`yarn.lock`、patches） | 4 | 166 | 137 | 工具链 |

## 2. 提交分类

### 2.1 破坏性重构（4 个）

| 提交 | 标题 | 破坏点 |
| --- | --- | --- |
| `279ac22` | `refactor!: move official services into the std namespace` | 官方服务键从 `foo` 改为 `std:foo`；`ctx.foo` → `ctx.std.foo`；拦截键同步改名 |
| `915e17a` | `refactor!: pass entry behaviour to fibers through options` | `ctx.registry.use(component, config, getOuterStack)` 第三个参数改为 `Fiber.Options`；删除 `internal/commit` 事件 |
| `27b9010` | `refactor!: rename getOuterStack to getStack` | 回调名与形参名全线改名 |
| `7e4387d` | `feat(server-proxy)!: forward to a prefix and map request headers` | `target` 从「目录」变成「前缀」；删除 `server.proxy` 服务；新增 `isolateOrigin`、`requestHeaders` |

`33dd8ee` 与 `0aa3e5c` 同时带 `!`，但它们只改 fs 的调用契约与 chunk 所有权，不动服务键或配置形状。

### 2.2 能力新增（11 个）

| 提交 | 内容 |
| --- | --- |
| `0bc6f0a` | `fs-memory`：内存文件系统 provider |
| `2ce387c` | fs entry 操作：`access`/`rename`/`remove`/`stat`/`mkdir`/`symlink`/`readlink`/`copy`/`list` + 一致性测试套件 |
| `33dd8ee` | fs 字符串按路径解析（`/` 与 `\` 都分隔、`.`/`..` 有目录语义），名字数组逐元素取字面 |
| `0aa3e5c` | fs 区间读（`ReadOptions.start/end`） |
| `f4c6b3a` | fs 原子写与独占写/复制（`atomic`、`exclusive`） |
| `a68681d` | fs 条件读写（`ifMatch`，基于 provider 的 `version`） |
| `6feacd0` | fs `DirRef`（`dir()`、`file()`、`list()`） |
| `285a949` | provider 的 `readlink()`/`realpath()` 返回对象 |
| `bea04de` | http `event-stream` 响应类型与 SSE 解析器 |
| `4fc11da` | http `noProxy` 选项 |
| `86f59e4` | 仅测试：shadow ctx 延长时的 def site（`#182`） |

### 2.3 缺陷修复（16 个）

| 提交 | 内容 |
| --- | --- |
| `f35ee3a` | timer 节流窗口改用单调时间 |
| `bdd4126` | timer 回调重入时保留尾部句柄 |
| `c9fe179` | include 定位无前置锚点的别名 |
| `32dd638` | fs watch 结束后丢弃排队的 ready 回调 |
| `bc94ca2` | fs-node 不监听 FIFO/socket/设备 |
| `38abad0` | hmr 在 Node.js 22 上的同步模块钩子 |
| `8dbd5d3` | loader 转移后的 entry 由新 group 释放 |
| `a15b178` | server 保留含 `?` 的完整查询串 |
| `65edc8f` | server 对畸形百分号路径回 400，并让捕获跨编码边界的路由不匹配 |
| `93bc78a` | server-static 在尝试扩展名之前重定向目录 |
| `3ef1c4b` | env 允许空的环境变量覆盖文件 |
| `5e2e6c1` | hmr 监听符号链接目标的真实路径 |
| `418a010` | server 在 public URL 中给 IPv6 主机加方括号 |
| `127e57e` | server 表单响应体只编码一次 |
| `e51673d` | http 便捷调用的方法名转大写 |

### 2.4 结构性调整（4 个）

| 提交 | 内容 |
| --- | --- |
| `d6de1c0` | `packages/fs` → `packages/fs/core`，`packages/fs-node` → `packages/fs/node` |
| `c358e27` | 下划线前缀成员改为 `private`（类型层面不破坏，但外部不能再直接触达） |
| `54cbf0f` | 版本号 `4.0.0-rc.13` → `4.0.0-rc.14` |
| `b1b8a44` | 仅文档：schema 描述翻译补全 |

### 2.5 工具链与发布（2 个）

| 提交 | 内容 |
| --- | --- |
| `5fcee75` | 升级 yakumo，删除 `.yarn/patches/*` |
| `eaf642c` / `6ad12cf` | 发布改为「带 `release` 标签的 PR 合并时触发」，只有 `cordis` 版本变化才打 tag |

## 3. 本项目的必跟项

以下变更无法「不跟」——不跟就是语义错误或编译失败：

| 编号 | 上游变更 | 不跟的后果 |
| --- | --- | --- |
| U1 | 官方服务键加 `std:` 前缀 | 与上游配置/文档/lib 互操作时键名不一致；`fs` 的拦截键改为 `std:fs` 后 baseUrl 拦截失效 |
| U2 | `Fiber.Options` 取代 `getOuterStack` 位置参数 | 调用点签名不兼容；`internal/commit` 事件消失，配置回写路径断裂 |
| U3 | `ctx.entry` → `ctx[Entry.key]` | 嵌套插件继承 entry 的隐式通道消失，include/loader 的归属判断要重写 |
| U4 | fs 包拆分与新配置语义 | 引用路径变化；`baseUrl` 语义从「基准」变成「目录基准」 |
| U5 | server-proxy `target` 变前缀 | 现有 `target` 配置会指向错误 URL |

## 4. 本轮与下游诉求的关系

下游 `KARDIA-ADOPTION-ANALYSIS.md` 提出 K1–K9 共 9 条诉求。逐条对照这 37 个提交：

| 诉求 | 上游本轮是否有对应变更 | 说明 |
| --- | --- | --- |
| K1 事件带 `FiberId` | 否 | `emit` 的 panic 捕获路径未改 |
| K2 `Bridge::emit_scoped` | 否 | bridge 不在本轮改动范围内 |
| K3 `BridgeOptions.call_bound` | 否 | `BridgeOptions` 仍只有 `timeout` |
| K4 `open` 由宿主注入 client 标识 | 否 | 未改 |
| K5 guest `invoke` 指向宿主服务 | 否 | `kardia-guest`/`kardia-hmr` 协议未动 |
| K6 单实例并发 export | 否 | 未动 |
| K7 host→guest `event` | 否 | 未动 |
| K8 guest→host export 外主动调用 | 否 | 未动 |
| K9 `ProcessGuest` 重启/退避 | 否 | 上游 Cordis 没有 guest 概念，此项属于 kardia 自有能力 |

结论：**上游 Cordis 与下游诉求几乎正交**。上游在收敛服务命名与 fs 能力，下游在要故障归属与并发。两件事需要分开排期。

## 5. 版本与互操作提示

- 上游版本进入 `4.0.0-rc.14`，仍是 RC，说明 `std` 命名空间与 `Fiber.Options` 还会继续演化。
- 本项目 `js/tests/server-parity/upstream.ts` 里硬编码了 `UPSTREAM_SHA`。跟进上游必须同步这个常量，否则 `server:prepare` 会因 SHA 不符而失败（脚本先比较再决定是否复用已有检出）。
- `js/tests/server-parity/README.md` 记录了 224/226 个对照测试的现状，跟进后需要重新计数。

## 核对命令

```bash
cd cordis
git log --reverse --format='%h %s' 36dc7fb..origin/main
git diff --numstat 36dc7fb..origin/main
git diff --stat 36dc7fb..origin/main -- packages/core packages/loader
```
