# 11 完整实现计划

本文汇总 `01` 到 `10` 的结论，把「跟进上游」与「落实下游反馈」编成一份可执行的计划。

## 0. 规划原则

1. **先修缺陷，再做新增。** 上游本轮修的一批真实 bug 在 kardia 全部存在，这些是纯收益，没有设计争议。
2. **改名集中做一次。** `std:` 前缀触及 30 多处但零语义风险，和其它变更混做会让 review 失效。
3. **破坏性变更有独立提交与迁移说明。** 尤其是 server-proxy 的 `target` 语义与 `ServerProxy` 的去留。
4. **fs 按消费者倒推，不做完整对齐。** kardia 没有外部 provider 生态，照搬上游 7000 行只会增加负担。
5. **下游 K6 到 K8 不做。** 协议级改动需要同步 Python SDK，收益不足以支撑本轮。
6. **每阶段结束都要能通过现有 CI。** 参照 `.github/workflows/ci.yml`：`cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo xtask arch`、`cargo test --workspace`，以及两条 no-default-features 组合。

## 1. 阶段划分

| 阶段 | 主题 | 依赖 | 建议拆成几个提交 |
| --- | --- | --- | --- |
| P0 | 缺陷修复（无设计争议） | 无 | 4 |
| P1 | 上游破坏性变更对齐 | P0 | 3 |
| P2 | 上游能力新增（按需） | P1 | 4 |
| P3 | 下游反馈落地 | P0、P1 | 3 |
| P4 | 验收与文档 | P0 到 P3 | 2 |

---

## P0 缺陷修复

这一阶段的每一项都是「上游已修、kardia 仍有」的真实错误，改动小、无争议。

### P0-1 查询串保留完整内容

**文件**：`crates/kardia-server/src/request.rs`

现状：

```rust
let mut parts = url.split('?');
let path = parts.next().unwrap_or_default().to_owned();
let query = url::form_urlencoded::parse(parts.next().unwrap_or_default().as_bytes()).into_owned().collect();
```

问题：`?a=1?b=2` 中第二个 `?` 之后的 `b=2` 丢失。

改法：按第一个 `?` 切分，查询部分取 `&url[path.len()..]`。

上游对照：`a15b178`，`this.query = new URLSearchParams(this.url.slice(this.path.length))`。

**测试**：`/x?a=1?b=2` 的 `query` 应保留 `?b=2`。

**验收**：新增测试通过，原有 server 测试不回归。

### P0-2 表单体 content-type 与 boundary 同步

**文件**：`crates/kardia-server/src/response.rs`

现状：

```rust
fn write_typed(&self, body: Body) {
    if let Some(content_type) = &body.1 {
        self.0.lock().headers.entry(CONTENT_TYPE).or_insert(HeaderValue::from_str(content_type).expect("..."));
    }
    self.set_body(body);
}
```

问题：`or_insert` 意味着第二次 `set_form_data()` 时 header 仍指向上一次的 boundary，而 body 用新 boundary，客户端解析失败。

改法：记录「上一次由表单体写入的 content-type」，仅当当前 header 等于它时才覆盖。

```rust
struct ResponseData {
    // ...
    /// 上一次为表单体生成的 content-type，用于识别「这个头是我写的」。
    form_type: Option<String>,
}
```

`set_form_data()` 走独立路径：新 boundary 与旧 `form_type` 相等时先删头，再写入新值。

上游对照：`127e57e`，`if (this.headers.get('content-type') === this._formType) this.headers.delete('content-type')`。

注意：不要为了省事改成「总是覆盖」。用户显式设置 `content-type: application/xml` 后再 `set_body`，那个头必须保留。

**测试**：连续两次 `set_form_data`，断言 header 的 boundary 与 body 一致；`set_header("content-type", ...)` 后 `set_body` 不被覆盖。

### P0-3 IPv6 主机在 public URL 中加方括号

**文件**：`crates/kardia-server/src/server.rs`

现状：`format!("http://{host}{prefix}")` 与三个端口分支。

改法：抽一个 `format_host(host) -> String`，含 `:` 时加 `[]`；`public_url()` 与启动日志共用。

上游对照：`418a010`。

**测试**：`host = "::1"`、`host = "2001:db8::1"`、`host = "0.0.0.0"`（通配转 127.0.0.1）、`host = "127.0.0.1"`。

### P0-4 timer 重入时保留尾部句柄

**文件**：`crates/kardia-timer/src/schedule.rs`

现状：

```rust
let timer = (self.trigger)(args);
let mut state = self.state.lock();
if state.disposed {
    if let Some(timer) = timer { timer.abort(); }
} else {
    state.timer = timer;      // 无条件覆盖
}
```

问题：`throttle` 带 `trailing: true` 时，回调内部再次触发会把新排的尾部句柄覆盖掉。

改法：

```rust
let timer = (self.trigger)(args);
let mut state = self.state.lock();
match timer {
    Some(timer) if state.disposed => timer.abort(),
    Some(timer) => state.timer = Some(timer),
    // 立即执行分支：`take()` 已 abort 旧尾部，这里不再保留
    None => {}
}
```

上游对照：`bdd4126`。

**测试**：`throttle` 回调在窗口内重入一次，断言尾部调用仍会执行、且 `dispose()` 能取消它。

### P0 完成标志

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask arch
cargo test -p kardia-server -p kardia-timer
```

提交建议：四项各一个提交，标题前缀 `fix(server):` 或 `fix(timer):`。

---

## P1 上游破坏性变更对齐

### P1-1 loader：entry 转移后的释放归属（含 `Fiber::leak`）

这是本计划里唯一的结构性缺陷修复，因为需要新增能力。

**新增**：`EffectHandle` 级别的解绑。

```rust
impl EffectHandle {
    /// 从所属 fiber 的效果列表中摘除这个效果，所有权转给调用方。
    /// 返回是否确实在该列表中。
    pub fn leak(&self) -> bool
}
```

实现要点：`EffectCell` 需要能从 fiber 的效果集合里移除自身，而 `Disposal` 仍可被调用方手动 dispose。落点在 `crates/kardia-core/src/effect.rs` 与 `state.rs` 的 `start_drain` 列表。

**修改**：`crates/kardia-loader/src/entry.rs` 增加持有者记录。

```rust
struct Holder {
    /// entry 的根 fiber（outer 或 inner）
    fiber: Fiber,
    /// 当前负责释放它的 group fiber
    owner: Fiber,
    /// 注册在 owner 上的释放效果
    release: EffectHandle,
}
```

`start()` 创建 inner fiber 后调用 `hold(fiber)`；`patch_context()` 在 context 重新挂到新 parent 后重建持有关系（先注册新 owner，再 `leak` 旧 owner）。

拒绝接管的条件与上游一致：目标 group 的 fiber 已被移除时直接返回。

上游对照：`8dbd5d3` 与 `915e17a` 的 `_hold` / `_release`。

**测试**（必须真实、无 mock）：

1. group A 下创建 entry E，把 E 转移到 group B。
2. 卸载 A。
3. 断言 E 的插件仍在运行（用它的服务或计数验证）。
4. 反向：卸载 B 时 E 被释放，A 的其它 entry 不受影响。

放在 `crates/kardia-loader/tests/` 下新增文件。

### P1-2 统一服务键为 std: 命名空间

**范围**：11 个 `Service::NAME` 常量、19 处 `Inject::new().with(...)`、2 处 `ctx.intercept(...)`、配置文件与示例、站点文档里的服务名。

**策略**：单向归一化，而不是硬改名。

在外部输入边界（配置解析、`Inject` 构造、`intercept` 元数据键）接受 `fs` 与 `std:fs` 两种写法，内部统一为 `std:fs`。理由：

1. 上游配置可直接使用，无需用户改写。
2. Rust 代码里裸写 `"std:fs"` 易错，归一化后至少不会「查不到」。
3. 拦截键 `intercept("fs", ...)` 必须归一化，否则 `baseUrl` 会静默失效——这是最难排查的失败模式。

```rust
/// 服务的规范键：官方服务带 `std:` 前缀，其它键原样。
pub fn canonical(key: &str) -> Key
```

只在三处入口调用一次，内部一律用 `Key`。这符合仓库规则「外部数据只在入口校验一次」。

**迁移**：`examples/*.yml` 与 README 的服务名同步展示 `std:` 形式；同时保留旧写法的可用性，不写兼容层代码（归一化本身就是兼容）。

**测试**：

1. `inject: [fs]` 与 `inject: [std:fs]` 都能启动依赖 fs 的插件。
2. `intercept: { fs: { baseUrl: ... } }` 与 `intercept: { "std:fs": {...} }` 效果相同。

### P1-3 server 路径与路由语义

三件互相关联的事，建议一个提交解决，因为它们都改 `pattern.rs` 与 `dispatch.rs`。

**(a) 畸形百分号回 400**

入口处（`crates/kardia-server/src/dispatch.rs` 或 accept 层）对 `req.path` 做一次严格解码校验；失败则直接产生 `400 Bad Request`，不进入路由。WebSocket upgrade 同样处理。

**(b) 路由捕获跨编码边界视为不匹配**

`pattern.rs` 现在：

```rust
if *byte == b'%' && (...非法...) {
    return Err(Error::msg("invalid URI parameter escape"));
}
```

改为返回 `Ok(None)`，让后续路由有机会。(a) 已经保证整条路径合法，这里只剩「路由字面量把转义切开」的情况。

**(c) 参数解码失败同样视为不匹配**

`percent_decode_str(&raw).decode_utf8()?` 的 `?` 改为 `match`，失败返回 `Ok(None)`。

上游对照：`65edc8f`。

**测试**：

1. 请求 `/%zz` 回 400。
2. 路由 `/a{b}` 配请求 `/a%2`，该路由不匹配并落到 404（不是 500）。
3. 路由 `/x/{p}` 配请求 `/x/%E4%B8`（不完整 UTF-8），不匹配。
4. 合法 `%E4%B8%AD` 正常匹配并解码为 `中`。

### P1 完成标志

```bash
cargo test -p kardia-core
cargo test -p kardia-loader --no-default-features
cargo test -p kardia-server
cargo test --workspace
```

---

## P2 上游能力新增

按消费者实际需要排序，可独立取舍。

### P2-1 fs：entry 操作（最小集）

只为三个消费者补齐。

**新增 `FsProvider` 方法**（带默认实现，未支持时报 `NotSupported`）：

| 方法 | 消费者 |
| --- | --- |
| `stat` | server-static（区分文件与目录）、include（只读判断） |
| `write` | include |
| `rename` | include |
| `remove` | include |
| `realpath` | hmr（符号链接真实路径） |

**新增 `FsExt` 方法**：`stat`、`write`、`rename`、`remove`、`realpath`、`exists`。

**`FileRef` 补 `write`**：include 现在自己拼临时文件（`_write_text`），补完可删掉那段。

**capabilities**：用一个轻量结构而不是布尔矩阵。

```rust
#[derive(Clone, Copy, Default)]
pub struct Capabilities {
    pub write: bool,
    pub stat: bool,
    pub realpath: bool,
}
```

未声明的能力由 `Fs` 层直接报 `NotSupported`，provider 不必实现空壳。

**不建议**本轮做：`symlink`、`readlink`、`copy`、`mkdir`、`readdir`、`access`、区间读、原子写、`ifMatch`、`DirRef`、内存 provider、conformance 套件。理由是没有消费者。

上游对照：`2ce387c`、`285a949`、`6feacd0`、`0bc6f0a`、`0aa3e5c`、`f4c6b3a`、`a68681d`。

**测试**：`crates/kardia-fs/tests/` 下新增 `entry.rs`，覆盖三种能力的有无组合。

### P2-2 server-static：目录先于扩展名

**文件**：`crates/kardia-server-static/src/serve.rs`

现状：`try_file()` 先 `read()`，非 Response 就进扩展名循环；`IsADirectory` 的重定向在外层才判断。

改法：`try_file()` 读到 `IsADirectory` 立即返回，不再尝试扩展名。

上游对照：`93bc78a`。

**测试**：磁盘上同时存在 `docs/` 与 `docs.html`，请求 `/docs` 应 301 到 `/docs/`。

### P2-3 http：noProxy

**文件**：`crates/kardia-http/src/config.rs`、`socket.rs`

新增字段：

```rust
pub no_proxy: Option<Vec<String>>,
```

在构造代理 client 前判断命中，命中则不设 proxy。算法移植上游 `bypassProxy()`：`*`、子域后缀、端口限定、IPv6 补括号、尾随点剥离。

上游对照：`4fc11da`。

**测试**：至少覆盖 `*`、`localhost`、`.example.com`、`127.0.0.1:8080` 与不命中样例。

### P2-4 server-proxy：前缀语义与头映射

破坏性，建议单独一个 minor 版本。

**(a) `target` 语义**：从「目录」改为「前缀」。三态：空串、scheme、以 `/` 结尾的 URL。

迁移说明必须写清：旧配置 `target: https://example.com/api`（无尾斜杠）现在非法（Schema 校验失败），需改成 `https://example.com/api/`。这是刻意的——静默兼容两种语义会产生歧义。

**(b) `requestHeaders`**：上游头名到来源头名的映射，含保留头校验。

**(c) `isolateOrigin`**：`Cookie`、`Set-Cookie`、`Clear-Site-Data` 过滤，加 CSP sandbox。

**(d) hop-by-hop 与 `Connection` 展开**：按 `Connection` 头声明的字段也视为逐跳。

**(e) `ServerProxy` 服务去留**：建议保留，并明确它返回「本 server 的 public URL」而不是「proxy 的 public URL」。上游删除它的理由在 kardia 不成立——kardia 的 `ServerProxy` 只是 `Server::public_url()` 的转发，没有额外语义，且下游已在使用。若最终决定删除，必须单独一个提交并在 CHANGELOG 里标 breaking。

上游对照：`7e4387d`。

**测试**：parity 套件里的 Proxy 对照需要按新语义重写；至少覆盖前缀拼接、越界拒绝（含 `%2F` 变体）、头映射消费、保留头报错、`isolateOrigin` 三件套。

### P2-5 parity 基线更新

若 P2 完成，需要同步：

| 文件 | 改动 |
| --- | --- |
| `js/tests/server-parity/upstream.ts` | `UPSTREAM_SHA` 改为 `6ad12cf2b39e7a22fe54b2f4568e948ecc778e97` |
| `js/tests/server-parity/README.md` | 更新测试计数与受影响的实现决策条目 |

注意：`server:prepare` 会校验已有检出，SHA 不符时报错而不是重置。更新常量后需要删除 `cordis/` 目录重新检出，或手动 checkout。

---

## P3 下游反馈落地

### P3-1 事件带 FiberId（K1）

**文件**：`crates/kardia-core/src/error.rs`、`events.rs`

现状（`error.rs:84`）是一个单字段元组结构，作为公开类型被 `lib.rs` re-export，且 `kardia-fs/tests/read.rs:147` 用 `downcast_ref::<Panicked>()` 断言：

```rust
#[derive(Debug, Clone, thiserror::Error)]
#[error("panicked: {0}")]
pub struct Panicked(pub String);
```

改为带来源的具名结构：

```rust
#[derive(Debug, Clone, thiserror::Error)]
#[error("panicked{}: {message}", .fiber.map(|f| format!(" in fiber {f}")).unwrap_or_default())]
pub struct Panicked {
    pub fiber: Option<FiberId>,
    pub message: String,
}

impl Error {
    pub fn from_panic(payload: Box<dyn Any + Send>) -> Self {
        Self::from_panic_at(None, payload)
    }

    pub fn from_panic_at(fiber: Option<FiberId>, payload: Box<dyn Any + Send>) -> Self {
        // 保留原有的 &str / String 解包与 "unknown panic" 回退
        Self::new(Panicked { fiber, message })
    }
}
```

`emit`、`bail`、`serial` 三处捕获点调用 `from_panic_at(Some(entry.fiber), panic)`；插件 apply 的捕获（`lifecycle.rs` 的 `execute()`）保持 `from_panic`，因为那里没有监听器归属。

这是对公开类型的破坏性变更（字段从元组变成具名），需要同步修改 `kardia-fs/tests/read.rs:147` 的断言。仓库规则允许破坏性重构，不需要兼容层。

**测试**：注册一个 panic 的监听器触发事件，断言日志里的错误带监听器的 `FiberId`；插件 apply 的 panic 仍为 `None`。

**下游收益**：可以把 `modules::adapters::deliver` 换回 `ctx.emit`。

**注意**：`FiberId` 定义在 `crates/kardia-core/src/key.rs` 并由 `lib.rs:34` 导出，`error.rs` 引用它不产生循环依赖，直接使用即可。

### P3-2 桥的会话可见性（K2）

**文件**：`crates/kardia-bridge/src/bridge.rs`、`session.rs`

```rust
pub struct SessionView<'a> {
    pub id: u64,
    pub client: &'a str,
}

impl Bridge {
    pub fn emit_scoped(
        &self,
        name: &str,
        payload: &Value,
        scope: impl Fn(&SessionView<'_>) -> bool,
    )
}
```

内部复用 `send_all` 的「收集 gone 再 `close_later`」逻辑。

**测试**：两个会话，只向 client A 广播，断言 B 收不到、A 收到；发送失败的会话被摘除。

### P3-3 宿主注入 client 标识（K4）

```rust
impl Bridge {
    pub async fn handle_as(
        &self,
        request: Value,
        client: &str,
        outbox: Option<Arc<dyn Outbox>>,
    ) -> Result<Value, BridgeError>
}
```

语义：`open` 请求的 `client` 字段被 `client` 参数覆盖；`close_client(&str)` 使用同一名字。

**测试**：请求里 `client: "forged"`、`handle_as(..., "real")`，断言会话的 client 是 `real`，`close_client("real")` 能关掉它。

### P3-4 恢复策略增强（K9 剩余部分，可选）

```yaml
recovery:
  mode: retry
  delayMs: 1000
  maxRetries: 5        # 新增，达到上限后转为失败不再重试
  backoff: exponential # 新增，linear 或 exponential
```

实现落在 `crates/kardia-loader/src/recovery.rs` 的 `RecoveryConfig` 与 `crates/kardia-core/src/recovery.rs` 的 `supervise()`。

注意：默认值必须保持「不重试」（`Manual`），符合仓库规则。新增字段不得改变现有 `mode: retry` 的行为（默认无限次、固定延迟），否则是隐性破坏。

### P3 不做项

| 项 | 理由 |
| --- | --- |
| K3 并发上限 | 待下游澄清是时长还是并发 |
| K5 宿主原生服务 invoke | 单独做收益有限，应与 K6 到 K8 一起 |
| K6 单实例并发 export | 协议级改动，需同步 Python SDK |
| K7 host 到 guest event | 依赖 K6 |
| K8 export 外主动调用 | 依赖 K6 |

---

## P4 验收与文档

### P4-1 完整验证

Rust 侧（与 CI 一致）：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask arch
cargo test --workspace
cargo clippy -p kardia-loader --no-default-features --all-targets -- -D warnings
cargo test -p kardia-loader --no-default-features
cargo clippy -p kardia-hmr --no-default-features --features process -- -D warnings
```

JS 侧：

```bash
cd js
bun install --frozen-lockfile
bun run check
bun run test
```

若改了 server 或 proxy：

```bash
bun run server:prepare
bun run test:server:reference
```

### P4-2 文档

| 文件 | 改动 |
| --- | --- |
| `README.md` 与 `README.zh-CN.md` | 服务键、proxy 配置、恢复配置的示例 |
| `examples/*.yml` | 服务名与新配置项 |
| `site/content/**` | 服务键与 proxy 语义（中英两份） |
| `js/tests/server-parity/README.md` | 基线 SHA、测试计数、实现决策 |
| `NOTICE` | 若新增改编自上游的代码（如 `bypassProxy`、SSE 解析） |

### P4-3 验收标准

| 阶段 | 验收 |
| --- | --- |
| P0 | 4 组新增测试各自通过，全量测试无回归 |
| P1 | transfer 测试通过；`inject` 与 `intercept` 两种写法均可用；路径四类用例符合预期 |
| P2 | 新增 fs 能力测试、static 目录测试、`noProxy` 测试、proxy parity 重写通过 |
| P3 | K1 的 `FiberId` 可见；桥的作用域广播与身份注入测试通过 |
| P4 | 上面全部命令通过 |

---

## 2. 风险与取舍

| 风险 | 说明 | 缓解 |
| --- | --- | --- |
| `std:` 归一化引入歧义 | 若某插件真的需要名为 `fs` 但非官方的服务，归一化会误伤 | 只对已知官方服务名归一化（白名单），其余原样 |
| `target` 语义变更破坏现有部署 | 旧的「目录」写法会校验失败 | Schema 报错信息里给出改写示例；CHANGELOG 标 breaking |
| `leak()` 作用于效果列表 | 需要改 `state.rs` 的 drain 逻辑，可能影响并发正确性 | 单独实现加专门的「泄漏后 drain」测试；不扩大到其它调用点 |
| `ServerProxy` 保留与上游分叉 | 上游已删，长期维护成本 | 明确文档说明「kardia 保留，语义是 server 的 public URL」 |
| parity 基线更新后计数变化 | 上游新增测试 | 以重新跑出来的结果为准，不沿用旧数字 |
| fs 能力只补最小集 | 后续消费者出现时要再补 | 在文档里写明「未实现清单」，避免误以为完整 |

## 3. 与仓库规则的符合性

| 规则 | 本计划的处理 |
| --- | --- |
| 允许破坏性重构，不留兼容层 | `std:` 用归一化（不是兼容层）解决；proxy 直接改语义不做双轨 |
| 只校验必要项，容忍未知字段 | `noProxy`、`recovery` 新字段都有默认值；不引入严格 Schema |
| 不为臆测的竞态或安全边界增加复杂度 | 拒绝「为 proxy 增加更复杂的越界检测」，按上游双重比较即可；K3 并发上限待澄清后才做 |
| 外部数据只在入口校验一次 | `canonical()` 只在配置解析、`Inject` 构造、`intercept` 三处入口调用 |
| 主函数保持正常流程 | P0 的修复都落在已有小函数里，不新增分支树 |
| 只注释非显然约束 | 计划里要求注释的点只有：`or_insert` 为何不够、`take()` 后为何不写回 `None`、`leak` 的所有权转移前提 |
| 不手改生成文件 | `generated.rs`（MIME）不动；若 fs 改动影响 MIME 生成，走仓库现有生成流程 |
| 不重复业务逻辑到测试 | transfer 测试用真实 loader 加真实 group，不 mock |
| 按现有工作流 | 用 `cargo xtask`、`cargo clippy`、`bun run`，不引入新工具 |
| 不过早抽象 | `format_host` 会被 `public_url` 与日志两处使用，符合提取条件；`canonical` 有三处入口，符合 |
| `lib.rs` 与 `mod.rs` 只声明与 re-export | 新增模块在 `lib.rs` 里只加 `mod` 与 `pub use` |

## 4. 建议的提交序列

| # | 提交 | 内容 |
| --- | --- | --- |
| 1 | `fix(server): keep the whole query string` | P0-1 |
| 2 | `fix(server): replace the form content type with the body boundary` | P0-2 |
| 3 | `fix(server): bracket an IPv6 host in the public URL` | P0-3 |
| 4 | `fix(timer): keep the trailing handle when the callback re-enters` | P0-4 |
| 5 | `feat(core): let a fiber leak an effect to its caller` | P1-1 的 core 部分 |
| 6 | `fix(loader): release a transferred entry with its new group` | P1-1 的 loader 部分 |
| 7 | `refactor!: canonicalize official service keys as std:` | P1-2 |
| 8 | `fix(server): reject malformed percent escapes and skip split routes` | P1-3 |
| 9 | `feat(fs): add stat, write, rename, remove and realpath` | P2-1 |
| 10 | `fix(server-static): redirect a directory before trying extensions` | P2-2 |
| 11 | `feat(http): add a noProxy option` | P2-3 |
| 12 | `feat(server-proxy)!: forward to a prefix and map request headers` | P2-4 |
| 13 | `test(parity): pin Cordis 6ad12cf and rewrite the proxy comparisons` | P2-5 |
| 14 | `feat(core): record the fiber of a panicking listener` | P3-1 |
| 15 | `feat(bridge): scope events to sessions and accept a host identity` | P3-2 与 P3-3 |
| 16 | `docs: publish the Cordis follow-up analysis and plan` | 本目录 |

## 5. 工作量估算

| 阶段 | 净增代码 | 测试组 | 主要不确定性 |
| --- | --- | --- | --- |
| P0 | 约 120 行 | 4 | 无 |
| P1 | 约 400 行 | 8 | `leak()` 对 drain 的影响 |
| P2 | 约 1200 行 | 15 | proxy 重写的 parity 数量 |
| P3 | 约 250 行 | 6 | K2 的元数据取舍 |
| P4 | 文档为主 | 全量 | 无 |

## 6. 首轮建议范围

若要控制风险，第一轮只做 P0 加 P1-1：五个提交、约 500 行，全部是无争议的缺陷修复，且 P1-1 是唯一有独立测试价值的结构改动。

`std:` 改名建议紧随其后单独做，因为它最琐碎但最容易验证（失败会立刻暴露为「服务查不到」）。

## 7. 与下游的协作顺序

| 顺序 | 动作 | 前置 |
| --- | --- | --- |
| 1 | 下游澄清 K3「上限」含义 | 无 |
| 2 | 下游澄清 K2 owner 隔离的归属 | 无 |
| 3 | kardia 完成 P3-1（K1） | P0 完成 |
| 4 | 下游据此删掉 `modules::adapters::deliver` | 第 3 步发布 |
| 5 | kardia 完成 P3-2、P3-3 | 第 3 步 |
| 6 | 下游修正 `KARDIA-ADOPTION-ANALYSIS.md` 里 K9 的过时描述 | 无 |
| 7 | 评估 K5 到 K8 是否单独立项 | 第 2 步有结论 |

## 核对命令

```bash
cd /root/code_workspace/kardia
sed -n '1,40p' docs/09-parity-matrix.md
ls docs
git log --oneline -5
```
