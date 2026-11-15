# Cordis Server 系列 Rust 重写计划

## 1. 目标与当前进度

将 `@cordisjs/server`、`server-acl`、`server-echo`、`server-proxy`、`server-static` 重写到 Kardia，保持固定上游版本的可观察行为，并通过同时运行上游和 Rust 实现的真实 HTTP、WebSocket、文件系统与插件生命周期测试验收。

故障隔离是硬性要求：一个插件失败不得使宿主或无关插件停止。恢复策略由使用者配置，支持自动重试、保持失败等待手动处理、交给宿主自定义处理，不能强制重启。三种策略都必须保证无关插件持续正常运行；只有选择自动重试时，才要求无需手动处理即可恢复，不能用手动恢复替代该模式的验收证据。

已开始执行重写。按本文件逐阶段实现、验证、提交推送和更新记录，全部完成后将内容重写为站点正式文档并删除本文件；未完成项以进度表及实际验证记录为准。

| 阶段 | 状态 | 提交与验证证据 |
|---|---|---|
| P0 分析、规范与计划 | 已完成并推送 | `b1da534`；[站点 CI 通过](https://github.com/licyk/kardia/actions/runs/43693689368)；详细验证见第 11 节 |
| P1 上游基准、对照框架与恢复契约 | 原生基准通过，WS 最小证明待完成 | `a3feda0` / `804782a`；三平台基准通过，HTTP 双端对照已接入 |
| P2 Server HTTP 核心与恢复监督 | HTTP 与恢复基础已推送，补充行为和 loader 集成待完成 | `b010642` / `4ac53b9`；50 HTTP 用例、4 故障用例及 18 双端对照通过，新代码原生 CI 待验收 |
| P3 WebSocket 与 Echo | 阶段验收通过 | `d2a1bc3`；迁移、隔离、双端及三平台 CI 通过，最终集成见 P9 |
| P4 ACL | 阶段验收通过 | `f907283`；15 个迁移用例、三种策略隔离、26 项双端场景及三平台 CI 通过 |
| P5 Static 所需文件服务能力 | 实现已推送，原生 CI 待验收 | `98711c3`；6 项读流/挂载/隔离测试及 watch/include/hmr 回归通过 |
| P6 Static | 实现已推送，原生 CI 待验收 | `f6e4e9e`；24 个迁移用例、流故障/三种恢复策略、完整 MIME 清单和共享配置对照通过 |
| P7 Proxy 所需 HTTP 客户端能力 | 未开始 | — |
| P8 Proxy | 未开始 | — |
| P9 集成、全量实机对照与故障验收 | 未开始 | — |
| P10 正式文档、站点发布与计划删除 | 未开始 | — |

## 2. 固定分析基线

- 分析日期：2026-11-15。
- Kardia：`f139819301845f9f00bf1d28244db3e6539778e5`，工作区版本 `0.2.3`，分支 `main`。
- Cordis：[`f6290dbbf1558cb3a870a58b1f97bacc0d3a9960`](https://github.com/cordiverse/cordis/commit/f6290dbbf1558cb3a870a58b1f97bacc0d3a9960)，[PR #344](https://github.com/cordiverse/cordis/pull/344)。本地 `cordis/` 与查询时的上游最新提交一致。
- 上游路径：`packages/server/{core,acl,echo,proxy,static}`。声明版本依次为 `1.7.0`、`1.0.1`、`1.1.1`、`1.2.1`、`1.2.1`；不以 npm 浮动版本代替源码基线。
- `cordis/` 已被根目录 `.gitignore` 忽略，是独立参考仓库。测试必须能从固定 SHA 重建环境，不能依赖该目录偶然存在，也不将其整个复制进本项目。
- 上游测试文件中有 105 个 `it(...)` 声明：core 57、ACL 15、proxy 9、static 24。Echo 没有自己的测试文件，proxy 现有测试未覆盖 WebSocket；105 是迁移清单的起点，不代表完整行为覆盖或当前已通过数。
- 本机工具版本已读取：Node `v24.19.0`、Bun `1.4.2`、Cargo `1.99.1`；未据此声称依赖安装、构建或实测通过。

执行期间固定基线；若决定跟进新上游，先记录新旧 SHA、相关行为变化和新增用例，再修改预期，不能让对照预期随上游主分支漂移。

## 3. 已阅读的实现及项目约束

上游入口：

- `cordis/packages/server/core/src/{index,body,listen}.ts`：路由、拦截、事件链、请求响应、监听与 WebSocket 生命周期。
- `cordis/packages/server/{acl,echo,proxy,static}/src/index.ts` 及各自的测试。
- `cordis/packages/http/src/index.ts`：proxy 实际调用的原始 HTTP 响应、WebSocket、配置拦截和清理。
- `cordis/packages/fs/src/{index,file,error}.ts`、`cordis/packages/fs-node/src/index.ts`：static 实际依赖的 URL、挂载、文件流与错误。

本项目入口：

- `AGENTS.md`；`CLAUDE.md` 引用同一规则。
- `crates/kardia-core/src/{service,events,effect,fiber,lifecycle}.rs`：`Svc` 调用者归属、类型化事件、效应清理、失败与恢复。
- `crates/kardia-fs/src/service.rs`、`crates/kardia/src/{catalog,lib}.rs`：已有文件监听及内置插件注册。
- `site/content/docs/development-{contributing,testing,design,lifecycle,status,concurrency}.mdx`、`guide-guests.mdx`。
- `.github/workflows/{ci,js,site,release}.yml`、`xtask/src/main.rs`、Cargo 与 Bun 清单。

执行规则：

1. 允许破坏性重构；调整受影响调用方，不保留仅为兼容存在的旧接口。
2. 外部配置、请求和拦截数据在边界做必要校验一次；内部传递已知类型，容忍未知字段。
3. 不为假设性的竞态或极端情况新增机制。本计划中的故障监督直接对应用户要求和已确认的能力缺口；超出范围的防御机制仍遵循 AGENTS.md 的事先说明要求。
4. 入口函数保留正常流程；复杂或有明确概念的逻辑才提取；只注释非显然约束。
5. `lib.rs` / `mod.rs` 仅声明模块、重新导出及承载文档/属性；执行 `cargo xtask arch`。
6. 使用现有 Cargo、Tokio、Bun、Vitest、xtask 和 GitHub Actions 流程。扩展现有测试与 CI，不另建测试框架或发布系统。
7. 锁文件和站点生成文件通过工具生成，不手改。保留 Cordis 的 MIT 归属，遵循已有 `NOTICE`。
8. 本计划不包含自动发版、打标签或发布 crates.io/npm；阶段提交推送与正式版本发布分开。

## 4. 实现结构与必要依赖

| 上游 | 拟新增 crate | 插件/服务与集成 |
|---|---|---|
| `@cordisjs/server` | `kardia-server` | `server`，服务键 `server`；监听、HTTP、WS、路由及类型化钩子 |
| `@cordisjs/server-echo` | `kardia-server-echo` | `server:echo`，依赖 `server` |
| `@cordisjs/server-acl` | `kardia-server-acl` | `server:acl`，依赖 `server` |
| `@cordisjs/server-static` | `kardia-server-static` | `server:static`，依赖 `server`、`fs` |
| `@cordisjs/server-proxy` | `kardia-server-proxy` | `server:proxy`，依赖 `server`、`http`，提供 `server.proxy` 的 Rust 对应访问方式 |
| proxy 的 HTTP 依赖 | `kardia-http` | 为 proxy 所需的 HTTP(S)、WS(S)、配置与拦截提供可替换的 `http` 服务 |
| static 的 FS 依赖 | 扩展 `kardia-fs` | 保留实际监听能力，补充 URL 解析、挂载、只读文件流、错误映射与调用者元数据 |

所有新 crate 继承工作区版本、lint 和发布配置；统一入口重新导出，`catalog.rs` 注册插件名和上表实际 npm 包名。不因旧目录使用 `@cordisjs/plugin-*` 别名而给新增包编造该前缀。

### 已确认的缺口

- `kardia-fs::Fs` 当前为文件监听服务，没有 `baseUrl`、`resolve`、provider mount 或文件读取。static 不得绕过 `fs` 直接硬编码本地磁盘，否则非本地 provider 和拦截行为会丢失。
- Kardia 没有 HTTP 客户端服务；proxy 不得把私有客户端藏在插件里，绕过服务依赖、替换与调用者上下文。
- 核心没有通用 `waterfall`。参考已有类型化钩子模式，在 server 内提供请求、路由请求和 upgrade 的可组合链；同步 route-check 使用适当的类型化拦截点。不得把 `serial` 当成支持 `next()` 的中间件链。
- 路由注册、钩子、连接归属使用 `Svc<Server>::caller()`；服务依赖从定义处解析，不能让请求的使用处覆盖提供者身份。多实例隔离必须实测。
- 当前锁文件没有可直接沿用的 HTTP/WS 栈。P1 在现有 Tokio 运行时上选择成熟传输库，记录版本、MSRV、流式能力和许可证；网络协议编解码交给库，Cordis 路由和生命周期语义由本项目实现。不得直接接受 Web 框架默认 HEAD、405、重定向或错误语义。
- 本轮不扩大为完整移植 HTTP 解码器生态、SOCKS 或 FS 写入/重命名/删除体系。支持五个目标包所需的完整依赖路径；未覆盖的辅助服务接口必须明确记录，不能声称已完整移植整个 `@cordisjs/http` / `@cordisjs/fs`。
- 生产实现为 Rust；TypeScript 仅承担真实上游驱动和测试。本轮不增加五套 `@kardiajs` 同名实现，也不额外扩展 Tauri/Guest 网络协议。

## 5. 必须保持的行为

### 5.1 Server 核心

- `host` 默认 `127.0.0.1`，`port` 必填且允许 `0`；`maxPort` 默认等于起始端口。仅地址占用或权限错误触发逐端口尝试，其他绑定错误直接失败。
- `publicUrl` 使用实际监听端口；显式值去尾斜杠后附加调用者 `intercept.path`；通配监听地址映射为 `127.0.0.1`，80/443 的 URL 行为按源码实测。不能把生成 https URL 误作监听器支持 TLS。
- 路由按注册顺序匹配；支持字符串参数、通配符、可选片段和正则路由。字符串匹配须对齐 path-to-regexp 的大小写、尾斜杠、捕获与解码行为；正则 API 用 Rust 类型表达，正则语言无法直接等同的部分逐项记录并解决，不能悄悄缩为简单 `:id` 路由。
- 参数解码中 `+` 转为空格；query 不进入 path 参数；拦截前缀按源码的字符串前缀处理，不擅自增加路径段边界规则。
- `all/get/delete/head/post/put/patch`、404、405、OPTIONS 204 及 `Allow` 的小写方法、顺序和 `*` 行为保持一致。GET 不自动接受 HEAD。
- 请求中间件、route-request 和 `next()` 保持短路与返回响应传播；响应通过显式状态或 body 标记为 claimed，单独设置 header 不等价于处理完响应。
- 未 claimed 的普通异常返回 500；请求验证失败返回 422 和 `issues`；已 claimed 的状态/响应不被覆盖。验证器允许异步，Rust 接口不模拟 JS 的类结构，但保留输入、结果和错误契约。
- body 支持字节、文本、JSON、表单与流，保留 content-type 覆盖规则及后写 body 覆盖前写行为。请求消费状态与响应读后仍可发送、并发读取分别建模；不能把流一律读完缓存来替代流式服务。
- `accepts` 内容协商、重复 query、重复 header、JSON 解析失败、非法参数编码及状态文本纳入补充对照。
- HTTP 字符串路由的 `intercept.routes["METHOD path"]` 覆盖调用时选项，保留未覆盖字段；不能把这项覆盖规则擅自套到上游未使用它的 WS/正则路由。

### 5.2 WebSocket 与 Echo

- WS 按路由顺序匹配，经过相同 route-check；无匹配返回 HTTP 404。可省略 handler 自动 accept；显式 handler 未 accept 时断开；accept 前异常返回 500，accept 后异常关闭连接。
- 每条 WS 路由记录连接；连接关闭后移除，释放路由关闭其连接，不影响其他路由。
- Echo 仅注册空路径 POST 与 WS，由 intercept 决定挂载路径；HTTP 保留请求 content-type 并流式回显，WS 保留文本/二进制类型和内容。

### 5.3 ACL

- `allowedHosts` 缺省/true 放行；false 只允许 loopback；列表允许 loopback 和列出的域名。大小写不敏感、忽略端口、支持方括号 IPv6；`.example.com` 匹配裸域和子域。
- 路由选项覆盖组件设置，不是将两个列表合并。
- 拒绝表示跳过当前路由；后续路由仍可能处理请求。全部被拒绝时返回 404，不自行返回 403，也不通过 405 泄露路由；HTTP/WS 都适用。

### 5.4 Static

- root 相对 `ctx.fs.baseUrl` 解析，baseUrl 随配置与拦截逐层解析；支持真实本地 provider 以及测试中实际注册的非本地 provider。
- 空路径/目录尝试 `index.html`、`index.htm`；无尾斜杠目录默认 301，Location 按上游构造；`redirect: false`、扩展名候选顺序与原文件优先行为一致。
- exclude 使用相对 root 的斜杠路径并匹配点文件；现有 `globset` 的语义不能直接假定等于 picomatch，须用共享样本对照。
- 仅 404 可按 fallthrough 继续；读取错误映射：缺失、非目录、目录和 InvalidPath 为 404，PermissionDenied 为 403，其他为 500；路径逃逸的处理按真实请求实测。
- 自定义错误页保留原状态，并不应用下载 disposition；错误页解析与普通文件处理的实际差别也要对照，不能暗自修正上游再声称一致。
- MIME 先内容探测，再文件名，最后 octet-stream；探测使用独立流且结束后取消。必须覆盖小块 PNG 和 192 KiB ZIP 等跨块/超出首块的识别，不能只探测首块。探测库覆盖范围与上游 `file-type` 的差异须列入矩阵并关闭，不能仅两种文件通过就宣称等价。
- `attachment` 的 UTF-8 文件名编码、`inline` 以及不配置时的 header 行为一致；空文件、首读失败、后续流错误、断开后的文件句柄释放都要测试。

### 5.5 Proxy

- target 作为目录，捕获路径在其下解析；query 重新按 URLSearchParams 序列化，不能假定原始 query 字节完全保留。
- HTTP 转发 method/header，仅 POST、PUT、PATCH 转发 body；调用原始 HTTP 接口，保留 4xx/5xx 响应，不能套用便捷方法的失败状态校验。
- 响应保留状态、body 与其余 header，但移除已解压后不再匹配的 `content-encoding` / `content-length`；真实 gzip 等压缩及 redirect 默认行为纳入对照。
- WS 将 http/https 转为 ws/wss，过滤握手专用 header，等待上游连接成功后才接受下游；保留文本/二进制，双向传播关闭与错误。
- 关闭码允许 1000 或 3000–4999，否则按上游转为 1000；保留上游错误导致的对应关闭行为，不自行给未 accept 的失败补成 502。
- `server.proxy.publicUrl` 随当前 server 与调用者 intercept 解析；替换 `http` 或 `server` 必须经依赖关系重建 proxy，不影响无关组件。

## 6. 强制故障隔离与可配置恢复：设计与验收

### 6.1 当前事实与文档修正

`crates/kardia-core/src/fiber.rs` 和 `tests/fiber.rs` 明确规定：`restart()` 不清除失败，依赖刷新也不会重试 Failed；`update` / `update_with` 或 `registry.replace` 才清除错误。`apply` 的 unwind panic 已被捕获，但这不能证明独立网络任务或所有用户回调已被监督。原 `guide-plugins` 和 `development-lifecycle` 的中英文页面错误地把 restart 列为失败恢复手段，本阶段已按源码修正。

`AGENTS.md` 的强制规范是后续验收标准，不能用它替代当前实现证据。此次需提供三种可配置的恢复策略并验证共同的故障隔离保证，不将策略选择接口标记为已完成，也不把自动重试作为唯一允许的行为。

### 6.2 恢复方案

1. 单次请求、验证、代理连接或文件读取的普通失败在该操作范围内处理，保持上游 HTTP/WS 契约；只要实例仍可服务，不重启整个插件。
2. 插件实例、监听循环或长期任务已失效时，将错误归属到拥有者 fiber；使用现有失败路径撤回服务并清理效应。用户 handler 的 panic 捕获同时覆盖调用 closure 和轮询其 future，不仅检查 Tokio JoinError。
3. 提供使用者可选的自动重试、保持失败等待手动处理、宿主自定义处理三种策略。未配置时计划保持现有的失败等待行为，不默认启用强制重启；P1 明确配置字段、默认值及库/catalog 两条加载路径，宿主自定义处理通过宿主注册的处理器接入，不能仅在 CLI 外包一层实现。
4. 自动重试模式在实例之外设置可复用监督逻辑，等待清理完成后使用最新配置的 `update_with(config, true)` 恢复，不调用无效的 restart，也不制造配置回写。同一实例同时最多一个恢复任务；该模式初始采用可配置的固定非零重试间隔（计划默认 1 秒），不引入复杂退避框架。配置错误先报告，不对原样无效配置循环重试。
5. 手动模式完成故障清理后保持 Failed，等待使用者显式更新配置或替换实现，不启动重试任务。自定义模式向宿主提供所属 fiber、错误及必要上下文，由宿主决定告警、恢复、替换或继续保持失败；宿主处理器自身失败也必须隔离并报告，不能暗中回退到强制自动重试。
6. 监督和宿主处理资源归属存活的管理作用域，不能随失败实例一起被销毁；明确禁用、释放、移除时取消待执行恢复。切换策略时取消旧策略的待执行动作；配置变更时读取新配置，不能用旧配置覆盖用户操作。
7. 任一策略下只暂停故障依赖子图；若服务恢复，让原有依赖调和恢复真实消费者。ACL/Echo/Static/Proxy 失败不得重启共享 Server；一个隔离 Server 失败不得停止另一个 Server。
8. 如需共享基础设施调整，限定为本项策略配置需求，增加真实生命周期回归测试；不要顺带改变现有 `restart` 语义，或让全部历史 Failed 插件未经选择无限重启。完成时文档明确策略入口、默认值和适用范围。
9. 上游行为对照负责协议与正常生命周期；Kardia 的故障隔离与可配置恢复策略独立验收，不把增强机制伪装为上游已经具备。三种策略下都必须证明无关插件正常运行。

进程内插件保证错误及 unwind panic 的隔离；`abort`、段错误和进程终止必须通过现有 process/WASM 边界按适用情况隔离。不得声称 catch_unwind 能保护宿主免于进程级崩溃，或为动态库提供不存在的隔离。此次共享改动须保留既有 guest 失败报告与 panic containment 行为。

### 6.3 必过故障矩阵

| 故障注入 | 故障插件/操作预期 | 同时必须证明 |
|---|---|---|
| apply 返回错误、unwind panic；占用监听端口后释放 | 自动模式条件满足后重试；手动模式保持 Failed；自定义模式遵从宿主决定 | 三种策略都保持宿主 PID 不变和独立服务持续响应；仅自动模式禁止测试主动调用恢复 API |
| HTTP handler/请求钩子 panic，失败发生在首次 await 前后 | 请求按协议失败；实例失效时按选定策略处理 | 同 listener 的健康插件路由继续服务；未失效的实例能处理后续请求 |
| WS handler panic、连接中断、转发任务失败 | 清理所属连接；实例失效时按选定策略处理 | 其他 WS 持续交换消息；其他 HTTP 路由持续响应 |
| Server accept/长期任务异常退出 | 撤回服务并按选定策略处理 | 其消费者等待服务；服务恢复后再就绪；独立 Server 及定时器始终不停止 |
| ACL/Static/Proxy 实例失败或依赖撤回 | 仅对应插件及真实消费者受影响 | 不重启共享 Server；同服务的其他插件保持工作 |
| 文件首读/流中错误、客户端取消、上游 HTTP/WS 不可用 | 当前操作失败、资源释放；后续正常操作恢复 | 无悬挂读流、未观察的任务失败或持锁等待 |
| 三种模式下连续三轮故障与按策略处理 | 自动重试、显式手动恢复、自定义处理分别验证；每轮都有清理证据 | 路由/监听器/连接/任务计数回到基线，无重复响应；无关插件持续工作 |
| 恢复等待期间 disable/dispose/remove、更新配置或切换策略 | 取消旧动作，遵循新配置/策略 | 旧实例/旧配置不复活，配置文件没有自动恢复噪声 |
| 宿主处理器返回错误或 panic；宿主决定暂不恢复 | 报告处理器错误并保持隔离；不擅自重启 | 无关插件持续响应，失败被记录，无强制重试回退 |
| process 客体退出/被杀，WASM trap（共享变更回归） | 既有隔离与报告保持有效 | 宿主及无关插件仍活；不把手动恢复当自动恢复证据 |

测试持续发送真实请求并观察独立插件的进度，不只检查进程存活或最后一次请求。故障通过真实测试插件、占用端口、真实断开的 upstream 和可控 provider 注入；不得只 mock 一个错误返回来替代整条生命周期。

## 7. 真实对照测试方案

### 环境与驱动

- 用同一套语义场景驱动固定 SHA 的上游进程和 Rust 测试宿主；适配器只负责注册等价路由、插件和报告地址，不实现路由匹配、MIME 判断或代理逻辑。
- Rust 集成测试移植上游 105 个测试声明，保留原名的 snake_case 便于追溯。另以已有 Vitest/进程宿主方式组织跨实现测试；建议放在 `js/tests/server-parity/`，增加专用配置和 package script，不混入 npm 发布包。
- 上游按自身 Node/Yarn/Yakumo 配置和锁文件准备，优先采用其 CI 已覆盖的 Node 24，并记录确切版本；需要构建时按上游 `core` 后 workspace 的顺序。执行命令在 P1 实测后写入，不把猜测的 Yakumo 参数写成已验证命令。
- Rust 宿主使用当前工作区源码；测试连接真实 loopback TCP/WS。除专用端口占用场景外使用端口 0，读取实际地址。静态文件使用临时真实文件及实际挂载的流 provider；代理目标由可控真实服务提供。
- readiness、错误、控制和退出均显式等待并设置测试截止时间；不能靠固定 sleep 假定服务已启动。结束时等待子进程和连接退出。

### 比较与留证

- 对比 HTTP 状态、状态文本、语义 header、body 字节，WS 消息类型/内容/关闭码，以及回调顺序、fiber 状态、资源清理和重建结果。
- 只规范化动态端口、临时路径、Date 等环境值及协议不敏感的 header 大小写/顺序；不删除关键 `Allow`、Location、content-type、content-disposition、query、关闭结果以掩盖差异。传输库自动生成的长度/分块差异逐项记录判定依据。
- 为每个场景记录 ID、上游文件和测试名、Rust 测试名、输入、两端结果及差异。无上游测试的行为标注为“补充场景”，预期来自真实上游运行。
- 发现差异先最小复现，区分协议行为、Rust API 表达与 Kardia 明确要求的隔离增强。未解决的功能差异不能标记通过；需要改变范围或上游可观察行为时先记录并取得用户决定，不以静默 skip 达成全绿。
- 最终证据记录 Kardia 实现提交、上游 SHA、OS/架构、Node/Rust 版本、命令、通过/失败/跳过数和 CI 链接。保存精简可复现清单及差异报告，完整日志作为既有 Actions artifact，不提交 node_modules、target 或生成站点。

### 平台与检查

- 本机先执行 Linux 真实对照；现有 Rust CI 的 Linux、Windows、macOS runner 分别运行相关网络/文件/生命周期测试和对照任务。它们是原生 runner 验证，不等同于用户个人设备验收。
- 上游测试、Rust 测试、两端黑盒对照、故障恢复测试四类结果分开报告。跨平台未执行或跳过就是未验收，不能由 Linux 编译结果推断。
- 修改 crate 阶段执行受影响的 `cargo test -p ...`，并执行 `cargo fmt --all --check`、相关 clippy 和 `cargo xtask arch`；集成阶段执行 CI 使用的 workspace 和 feature 检查。
- 如果改到 `kardia-core` / `fs`，覆盖依赖的 loader/include/hmr 生命周期与监听回归。桥和 JS 产品无改动时不额外跑无关产品测试；对照驱动自身须类型检查。
- 扩展 `.github/workflows/ci.yml` 的现有平台任务准备上游和对照环境，不新建重复发布工作流。纯报告/站点提交按既有路径过滤执行。

## 8. 分阶段交付及退出条件

每阶段可按可审查范围拆成多个提交，但每个标记完成的部分都必须已验证并推送。

| 阶段 | 实施内容 | 必须达到的退出条件 |
|---|---|---|
| P0 | 修正文档；写入 AGENTS 强制规则；建立本计划 | 当前能力与目标分开；计划和规则可追溯；文档检查通过；提交推送 |
| P1 | 固定上游；运行基准；建立场景清单、真实双端驱动和故障样例；确定传输依赖与监督入口 | 上游 105 项基准结果有据；Echo/WS proxy 等补充行为有记录；HTTP/WS/stream/恢复最小验证明确依赖选择；所有未完成 Rust 场景保持未完成状态 |
| P2 | 新建 server；监听、路由、请求响应、钩子、调用者效应；实现恢复策略选择与自动模式监督 | 相关 core HTTP 用例与实机对照通过；HTTP 故障不拖垮其他插件；监听任务故障的三种策略均验证；直接嵌入和 catalog 路径都验证 |
| P3 | WS 路由、连接归属、Echo | WS 核心和 Echo 文本/二进制/流式回显对照通过；单插件 WS 故障隔离、恢复和释放通过 |
| P4 | ACL 与路由选项覆盖 | 上游 15 个声明对应用例及补充项通过；拒绝/fallthrough/WS 一致；实例失败不影响其他路由 |
| P5 | FS 的 URL/baseUrl、挂载、文件只读流、错误码及本地 provider | static 需要的接口真实可用；非本地 provider 能实际服务；旧 watch/include/hmr 回归通过；流关闭和 provider 隔离验证通过 |
| P6 | Static 配置、路径、候选、MIME、错误页、下载及流清理 | 上游 24 项与补充矩阵通过；不能以 MIME 子集代替完整审计；实例故障遵从三种恢复策略，其他插件始终继续运行 |
| P7 | HTTP 客户端服务及配置/拦截、流式响应、解压与 WS 生命周期 | 实际 HTTP(S)/WS(S) 对照、证书测试夹具、客户端替换和失败隔离通过；不声明辅助生态全面完成 |
| P8 | Proxy HTTP/WS、server.proxy 与资源清理 | 上游 9 项与 WS/压缩/header/query/关闭码补充项通过；上游恢复后健康实例可继续请求，失败实例按选定策略处理；故障不拖垮共享 Server |
| P9 | facade、catalog、CLI 配置、可运行示例；全矩阵验收 | 五包完整对照、三平台和故障矩阵通过；workspace/feature 检查通过；无未说明差异；每阶段推送与远端 SHA 有记录 |
| P10 | 将计划改写为中英文正式文档、更新导航/索引/README/开发状态，完成网站验证，最后删除计划 | 正式内容与代码对应；证据已迁入；站点检查与部署验证完成；本文件删除并推送 |

先完成核心再做消费者；static 与 proxy 的依赖分别先补齐。任何阶段不能以增加空 crate、只通过编译或省略任一恢复策略的隔离验收作为完成依据。

## 9. 提交、推送、进度记录

1. 开始每部分前检查当前分支、远端、工作区与本文件进度；保留他人的修改。
2. 实现和必要测试完成后，只暂存本部分文件；执行差异检查和上述必要验证，使用清晰提交信息，例如 `feat(server): ...`、`test(server): ...`、`docs(server): ...`。
3. 提交并推送当前授权分支，不强推、不覆盖他人提交；分别核实本地 commit、推送成功与远端 SHA。
4. 推送实现后更新本文件：写入实现提交 SHA、检查命令/结果、CI 链接与状态、原生验证、剩余问题；将进度文档作为单独提交推送。文档只记录已知结果，不记录自己的尚不存在的提交 SHA。
5. 代码阶段在 CI 尚未完成时可记为“实现已推送，验收待定”，不能记为完成。先修复相关 CI 失败再关闭阶段；不以被较新推送取消的运行作为成功证据。
6. 根目录 Markdown-only 进度提交按现有过滤不启动 Rust CI；证据绑定真正的实现 SHA。如果最后文档提交没有 Rust run，保留最近实现提交的已通过记录并说明范围。
7. 如实区分上游基准通过、Rust 自测通过、双端对照通过及原生平台通过；仅文档阶段不声称运行时能力已实现。

后续每次追加：

```text
日期 / 阶段：
完成范围：
实现提交 / 远端 SHA：
本地检查与结果：
上游 SHA / 对照场景与结果：
故障隔离与三种恢复策略的验证证据：
原生平台 / CI 链接与状态：
未执行、失败、差异及下一步：
```

## 10. 完成后文档整理与删除计划

正式内容放进已有 `site/content/docs/`，中文 `.mdx` 与英文 `.en.mdx` 同步：

- `guide-server`：安装/启用、HTTP/WS 注册、中间件、四个组件配置、intercept、可运行示例。
- `reference-server`：API、默认值、路由/响应/ACL/static/proxy 精确行为及上游固定来源。
- `development-server`：最终架构、文件与 HTTP 依赖的实际支持范围、故障监督、资源归属、对照方法、平台证据和明确差异。
- 更新 `development-testing`、`development-status`、`development-contributing` 及相关 FS/服务参考，移除“尚未实现”的旧描述时必须有对应验收证据。
- 更新 `meta.json`、`meta.en.json`、指南/参考/开发索引和两个 README 的插件与 crate 清单；依据最终清单与源码，不按本计划预先宣称已实现。

内容必须按使用者和维护者需要重写，不能把带待办与提交流水的计划直接复制到网站。保留基线、决策理由、可复现命令和验收结论；不保留已过时的拟议接口。

先提交并推送正式文档，通过站点既有 `bun run check`、`bun run test`、`bun run build` 与 `site.yml`；查看新页面、导航、双语和部署结果。确认长期证据已迁移后再单独删除本文件、检查无残留链接、提交推送。没有完成五包验收或文档发布时，不提前删除计划。

## 11. 进度记录

- 2026-11-15 / P0：完成固定上游代码和本项目集成点分析；确认 FS、HTTP、waterfall 类型化适配与自动恢复缺口；修正中英文插件、生命周期及元理论页面的失败恢复描述，参与开发页面补充规范说明，添加 AGENTS.md 强制规则。
- 本地验证：`git diff --check`、站点 `bun run check` 通过；`bun run test` 的 7 个文件、41 项测试全部通过。`bun run build` 完成编译后在预渲染启动失败，原因是现有进程占用 3000 端口；未停止该进程或修改站点配置，将以既有站点 CI 验证完整构建。
- 提交与推送：`b1da534912aa7c474738a28672feb523a7336a94`，`docs(server): plan rewrite and require plugin failure recovery`；已推送 `origin/main`，推送后核实远端 SHA 与本地一致。
- [Documentation site #43693689368](https://github.com/licyk/kardia/actions/runs/43693689368) 针对上述提交执行并成功完成，包含类型检查、测试、完整构建和 `gh-pages` 推送。本地端口冲突不再阻塞文档构建验收；网站托管侧的缓存刷新及用户浏览器访问未独立验证。
- 本进度记录按约定在上述提交推送后另行提交推送。Server 实现、基准测试、对照与故障注入尚未开始；本次无 Rust 代码变化，未运行 Rust 测试。

### P0 规范修订：恢复策略由使用者选择

- 2026-11-15：按用户修正，撤销早期“故障后必须自动恢复”的要求；本文件及 AGENTS.md 的现行要求为强制故障隔离、可配置恢复策略。支持自动重试、保持失败等待手动处理、宿主自定义处理，三者都须保证无关插件正常运行。上面的首次发布记录仅保留历史验证证据。
- 已同步修改中英文站点文档、实施步骤和故障矩阵；未配置时计划保持失败等待，自动重试仅在选择该模式时启用。宿主处理器故障和策略切换也纳入验收。
- 本地 `git diff --check`、站点 `bun run check` 及 7 个文件共 41 项测试通过。本次只修改规范、计划和文档，未改变运行时或执行 Rust 测试。
- 规范修订提交 `22e1bc470a893f0cfa98baf851a1c3aca3d0cc2e` 已推送 `origin/main`，远端 SHA 已核实一致；[站点 CI #43694079884](https://github.com/licyk/kardia/actions/runs/43694079884) 成功完成，包含完整构建和站点产物推送。此验证记录另行提交推送。

### P1 基准框架

- `a3feda06b31c67321c9346c750934ec8f0df8a0e` 已提交推送并核实远端 SHA。固定上游 SHA、可重建准备脚本、105 项生成清单及 22 项真实 HTTP/WS 补充基准位于 `js/tests/server-parity/`；现有 Rust CI 增加三平台基准任务。
- 本机验证：Yarn immutable 安装、`corepack yarn build core`、`corepack yarn build`、`corepack yarn test server server-acl server-proxy server-static` 通过（4 文件、105 项）；`bun run server:inventory` 生成 105 项，`bun run check`、`bun run test:server:reference` 通过（22 项）。这些结果仅证明上游基准，不代表 Rust 对照完成。
- 传输选型已查验：Hyper 1.12、tokio-tungstenite 0.30、Reqwest 0.13.5；来源、MSRV、许可证和恢复契约见基准 README。三平台 CI 及 Rust 传输/恢复最小证明待后续验证，不提前关闭 P1。
- 基准首轮 CI：Linux/macOS 基准及全部三平台 Rust 检查通过，Windows 原始 105 项通过，但生成清单的字节比较受 CRLF checkout 影响失败。`804782a` 修正检查时的换行规范化，已推送；不手改生成清单，不将该失败作为完成证据。

### P2 恢复基础

- `b010642` 已提交推送：新增 `Fiber::set_recovery`，默认手动保持失败，可选限频自动重试或宿主回调；监督跨实例存活，随 fiber 退役取消，切换策略取消旧动作。宿主同步/异步 panic 被捕获并记录；无根上下文重置，无自动配置回写。
- 本地 `cargo test -p kardia-core`、`cargo clippy -p kardia-core --all-targets -- -D warnings`、`cargo xtask arch` 通过。6 项新故障测试验证重复失败、手动/宿主策略、处理器 panic、取消恢复和配置错误不循环重试；网络持续服务与 loader 配置集成仍待 P2 后续。
- 上述恢复基础与 Windows 清单修复已通过 [CI #43694925630](https://github.com/licyk/kardia/actions/runs/43694925630)：Linux、Windows、macOS 的 Rust 和上游基准任务均成功。

### P2 HTTP 基础与双端对照

- `4ac53b9` 已提交推送并核实远端 SHA。新增 `kardia-server` 的实际 TCP 监听、HTTP 路由、中间件、路由钩子、参数匹配、请求验证及可分叉响应流；路由和钩子由调用方效应持有。路径编译遵循固定版本 path-to-regexp，NOTICE 保留归属。
- 本机 `cargo test -p kardia-server` 通过：50 个上游 HTTP 声明对应测试、4 个故障隔离测试；clippy、格式、架构和差异检查通过。
- 三种恢复策略均在三轮真实插件故障期间持续请求独立健康路由，验证故障路由清理和恢复后数量回归；同步及异步 handler panic 返回 500 后健康路由仍可服务。自动模式未调用手动恢复 API。
- `bun run check` 及双端测试通过：22 项上游补充基准 + 18 项真实 HTTP 对照。首轮发现适配器误把 `res.text` 对应成直接 body 写入导致 content-type 差异，已修正等价注册方式，未放宽比较规则。
- 现有三平台基准任务增加 Rust 宿主构建并运行双端测试，新提交 CI 尚待验收。P2 仍缺表单、内容协商等补充行为、loader 策略配置和监听任务完整故障矩阵，不能标记完成。

### P2 声明式恢复配置

- `253dc1f` 已提交推送并核实远端 SHA：entry 的 `recovery` 支持 `{ mode: "manual" }`（默认）、`{ mode: "retry", delayMs: 1000 }`、`{ mode: "host", handler: "name" }`；宿主通过 `Loader::register_recovery_handler` 注册处理器。未知配置字段保留，未注册的处理器报告错误，不运行插件或暗中改成自动重试。
- 新增 5 项 loader 测试通过：重复失败无配置回写、默认手动与宿主决定、禁用/切换取消、未注册处理器及边界校验；loader 全套默认/无默认 feature 测试、相关 clippy、格式和架构检查通过。
- Server 故障用例增至 5 项，包括 catalog 创建占用端口的隔离 Server，三种策略下释放端口并验证恢复，同时独立 Server 持续提供真实 HTTP 服务。该实现的原生 CI 尚待验收。

### P2 HTTP 补充语义

- `5a4615a` 已提交推送并核实远端 SHA：补齐 URL-encoded/multipart 表单、Blob、响应状态文本、Accept 协商及请求消费状态；响应保留可读分支和发送分支，客户端取消会释放数据源。
- HTTP 双端对照扩大至 42 项，加原有 22 项上游补充基准共 64 项，本机全部通过；新增路径可选组/混合通配符/引号与 Unicode 参数/正则前瞻、表单文件、协商和错误 JSON 场景。保留逐字节对照并修复 JSON 字段顺序、Blob MIME 小写差异。
- Rust 当前为 50 个 HTTP、5 个隔离、3 个流测试通过；类型检查、clippy、格式、架构和差异检查通过。路径编译依赖的实际锁定版本为 path-to-regexp **8.4.2**，已修正代码注释与 NOTICE 的错误版本标注。
- 较早 HTTP 提交的 CI 被后续实现提交取消，不能作为成功证据；最终原生验收继续跟踪较新实现。P2 的完整监听故障矩阵与集成验收仍需收尾，随后继续 WS/Echo。
- `5a4615a` 的 [CI #43696501141](https://github.com/licyk/kardia/actions/runs/43696501141) 已成功完成：三平台 Rust、上游原始 105 项、补充基准和 HTTP 双端对照全部通过；同提交 JS CI 也通过。

### P3 WebSocket 与 Echo

- `d2a1bc3` 已提交推送并核实远端 SHA。加入延迟 accept、upgrade 钩子、路由级连接归属和释放，新增 `kardia-server-echo` 的流式 POST 与 WS 文本/二进制回显。使用 tokio-tungstenite 0.30，协议帧由库处理；保留默认 100 MiB 上限和首个子协议选择。
- 本机 7 个原始 WS 对应用例全部通过；新增接收前/后 panic 隔离及 Echo 三种策略、各三轮故障测试通过。测试期间健康 HTTP 路由及同一健康 WS 连接持续交换数据，恢复后路由数量回归，释放后连接清理。
- 本机 Server 共 67 项 Rust 测试通过；69 项 JS 网络测试通过（22 上游补充基准、42 HTTP 双端、5 WS/Echo 双端）；WS 对比消息类型、字节、子协议、关闭码和原因。格式、相关 clippy、架构、类型及差异检查通过。
- 此提交原生 CI 待验收；P9 继续补完整集成矩阵。尚未完成 ACL、Static、HTTP 客户端及 Proxy，计划保留。
- `d2a1bc3` 已通过 [CI #43697257998](https://github.com/licyk/kardia/actions/runs/43697257998)，全部三平台 Rust 和网络基准/对照任务成功；同提交 JS CI 也成功。

### P4 ACL

- `f907283` 已提交推送并核实远端 SHA。新增 `kardia-server-acl`，按上游实现 loopback、域名列表、点前缀子域、端口/大小写/IPv6、路由选项替换及拒绝后继续匹配；HTTP/WS 使用同一检查入口。
- 本机 15 个迁移用例及三种策略的三轮故障测试通过（共 16 项）；故障期间公共路由持续服务，Server 绑定身份未变化，恢复后 ACL 效应数量回归基线。
- 新增 26 项双端场景，每项均比较真实 HTTP 和 WS 握手；总 JS 网络测试 95 项通过。相关 clippy、格式、架构、类型及差异检查通过。新实现三平台 CI 待验收，继续 FS/Static。
- `f907283` 已通过 [CI #43697734353](https://github.com/licyk/kardia/actions/runs/43697734353)：三平台 Rust、原始上游及双端测试成功；同提交 JS CI 成功。

### P5 Static 所需文件服务

- `98711c3` 已提交推送并核实远端 SHA。扩展 `kardia-fs`：分层 baseUrl、URL 解析、最长前缀挂载、类型化错误、调用方所属的只读流；新增显式 `fs:local` provider。原生 watch 保持既有行为，本轮不移植 FS 写入生态。
- 本机 6 项读取/挂载测试通过：真实本地文件、非本地流 provider、动态挂载、调用方释放/读取取消/provider 释放、同步及轮询 panic，以及三种恢复策略下独立 provider 持续可用。流采用有界传递，释放等待资源退出。
- FS、include、HMR 全套回归，相关 clippy、格式、架构、差异检查及 facade 编译通过。复用核心 panic 转换入口，未改变其错误语义。此提交三平台 CI 待验收，继续 Static。
- 首轮 [CI #43698822193](https://github.com/licyk/kardia/actions/runs/43698822193)：Linux/macOS Rust 和全部三平台网络对照成功；Windows Rust 在 FS 断言失败，实际原生系统把“父路径是文件”报告为 NotFound，而 Unix 为 NotADirectory。`931a8a9` 将该测试按原生平台区分，已提交推送并核实远端 SHA；本地 FS 6 项通过，修订的原生 CI 待验收。未改动错误映射，也未将首轮 CI 标记成功。

### P6 Static

- `f6e4e9e` 已提交推送并核实远端 SHA。新增 `kardia-server-static`，通过真实 FS 挂载实现 root、索引、扩展名候选、重定向、exclude、错误页、下载 header 和流式响应；检测独立持有可取消文件流。
- 本机 24 个上游声明对应测试通过，另 2 项测试覆盖 provider 同步/轮询 panic、首读/后续流错误、取消，以及三种策略各三轮实例失败。独立健康插件持续响应，清理后流计数归零，路由回到基线。
- MIME 按固定 `file-type` 20.5.0 识别顺序生成简单规则，复杂容器使用 Rust 流读取；固定样本库 SHA `7fc6d9ff7cc01987aa73006757792a5ca198023a` 的 254 个文件加两个补充样本，通过真实双端 Static 覆盖全部 171 种 MIME。测试去除扩展名，比较状态、header 和完整 body 摘要；1,180 个扩展名映射从固定依赖生成，CI 检查生成结果。
- picomatch-rs 0.1.0 经过 26 组配置、每组 15 个路径的共享对照；发现并修复上游默认 POSIX 选项对 `[!...]` 的差异。空文件重复轮询造成连接中断也已修复；错误页路径与普通路径的不同解析规则另有真实对照。
- 本机 JS 全套 123 项通过，随后新增错误页解析对照单项通过（当前清单共 124 项）；Static Rust 26 项、相关 clippy、格式、架构、类型、生成检查及差异检查通过。Native FS 错误码已加入双端场景。新提交三平台 CI 待验收，尚未完成 HTTP 客户端、Proxy 和最终集成。
