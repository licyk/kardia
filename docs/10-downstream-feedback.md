# 10 下游反馈分析

分析对象：`/root/code_workspace/Hanafubuki-dev/KARDIA-ADOPTION-ANALYSIS.md`（263 行，9 条诉求 K1–K9）。

该文档的定位是「为什么本项目没有完全采用 Kardia」，并把理由归成上游可处理的差距项。本文逐条核对它在 kardia 0.3.0 当前代码下的成立情况，并给出应对结论。

## 0. 下游的整体判断

下游把采用程度分成五层：

| 层 | 采用程度 | 下游文档判断 |
| --- | --- | --- |
| 前端插件宿主 | 完全采用 | 每窗口一个 root，每模块/扩展一个插件 |
| 后端模块宿主 | 完全采用 | `ModuleHost` 跑模块行 |
| 宿主与窗口 IPC | 部分采用 | 只用 `kardia-bridge` 的服务/事件/会话 |
| 模块间通信 | 没有采用 | 用自建 `modules::adapters::deliver` |
| 扩展后端进程 | 没有采用 | 普通进程加自定义 JSON-RPC，非 `kardia-guest/2` |

并判定「完全采纳」的工作量集中在后三项，不在「扩展性」。

这个判断与 kardia 现状一致，可以接受。下面逐条核对。

## K1 事件携带 FiberId（下游 P0）

### 下游主张

`Error::from_panic` 只取 panic 字符串，不带 `FiberId`；`ListenerEntry` 本身有 `fiber: FiberId`，信息存在只是没传进错误。所以本项目不能用 `ctx.emit`，只能用自建 `deliver`。

### 核对

`crates/kardia-core/src/error.rs`：

```rust
pub fn from_panic(payload: Box<dyn std::any::Any + Send>) -> Self {
    let message = payload.downcast_ref::<&str>().map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into());
    Self::new(Panicked(message))
}
```

`crates/kardia-core/src/events.rs` 的 `emit()`：

```rust
if let Err(panic) = catch_unwind(AssertUnwindSafe(|| listener(event))) {
    self.logger().log_error(&Error::from_panic(panic));
}
```

下游描述准确：panic 路径确实丢失了 `entry.fiber`。

### 结论

成立，且成本低。实现方式：

```rust
// error.rs：新增带来源的构造函数
pub fn from_panic_at(fiber: Option<FiberId>, payload: Box<dyn Any + Send>) -> Self

// Panicked 变体改为带字段
Panicked { fiber: Option<FiberId>, message: String }
```

`emit`、`bail`、`serial` 三处捕获点都带上 `entry.fiber`。

`Error` 是 `Arc<dyn StdError>` 包装，给 `Panicked` 变体加字段不破坏现有 `is::<T>()` 与 `downcast_ref` 用法。

工作量：`error.rs` 加 `events.rs` 两个文件，约 40 行含测试。

### 建议

采纳，作为 P0。这是本轮 kardia 唯一能立刻减少下游一层自建机制的改动。

## K2 Bridge::emit_scoped(predicate)（下游 P1）

### 下游主张

`Bridge::emit` 是 `broadcast`，无法按会话过滤；下游的 owner 隔离只能在桥外面用 `RowEvents` 加会话 scope 实现，每新增一类事件都要记得穿过过滤层，漏一处就越权。

### 核对

`crates/kardia-bridge/src/bridge.rs`：

```rust
pub fn emit(&self, name: &str, payload: &Value) {
    self.hub.broadcast(&HostMessage::Event { name, payload });
}

fn broadcast(&self, message: &HostMessage<'_>) {
    self.send_all(self.state.lock(), message);
}

fn send_all(&self, mut state: MutexGuard<'_, HubState>, message: &HostMessage<'_>) {
    let gone: Vec<u64> = state.sessions.values()
        .filter(|session| !session.send(message))
        .map(|session| session.id)
        .collect();
    ...
}
```

下游描述准确：`send_all` 遍历全部会话，没有谓词参数。

### 结论

成立。实现时要注意两点。

第一，谓词不应直接暴露 `Session`。`Session` 内部有 `outbox`、`fiber` 等句柄，暴露后下游会与内部结构耦合。提供一个只读视图：

```rust
pub struct SessionView<'a> {
    pub id: u64,
    pub client: &'a str,
}

pub fn emit_scoped(
    &self,
    name: &str,
    payload: &Value,
    scope: impl Fn(&SessionView<'_>) -> bool,
) {
    self.hub.broadcast_scoped(name, payload, &scope);
}
```

`Session` 已有 `client: String`（`session.rs:38`），足够下游按 client 过滤。

第二，必须保留「发送失败即摘除会话并异步关闭」的既有行为，否则谓词过滤会让失效会话永久留在表里。实现上让 `send_all` 与新的 `send_some` 共用同一段「收集 gone → `close_later`」逻辑。

工作量：`bridge.rs` 约 60 行含测试。

### 建议

采纳，作为 P1。但下游真正想要的是「owner 隔离由桥保证」，而不只是「能过滤」。若 `SessionView` 只暴露 `client`，下游仍需自己维护 client 到 owner 的映射，等于把过滤层换个位置。要真正消除，需要在会话上附加一段宿主提供的元数据，桥按该元数据过滤。这一点的取舍列入文末待确认项。

## K3 BridgeOptions.call_bound（下游 P1）

### 下游主张

`Bridge::emit` 与调用都没有「上限」语义，只有超时；下游在外部加了 `BRIDGE_CALL_BOUND`（10 秒）。

### 核对

`BridgeOptions` 只有 `timeout`：

```rust
pub struct BridgeOptions {
    /// How long a call into a client may take before it fails with `Timeout`.
    pub timeout: Duration,
}
```

`session.rs:70` 的调用等待：

```rust
match tokio::time::timeout(self.timeout, rx).await {
    ...
    Err(_) => Err(BridgeError::timeout(
        format!("{service}.{method} did not answer within {:?}", self.timeout),
    )),
}
```

也就是说，kardia 已经有「超时即失败并返回 `timeout` 错误」的语义，`BridgeOptions.timeout` 可配置。下游说的「外层加 10 秒上限」与 kardia 的 `timeout` 是同一类机制，只是下游没有把 `BridgeOptions.timeout` 设成 10 秒。

### 结论

部分不成立。需要区分两种「上限」：

| 含义 | kardia 现状 |
| --- | --- |
| 单次调用最长等待 | 已具备，`BridgeOptions.timeout` |
| 同时在途调用数量 | 不具备 |

如果下游指的是前者，只需把 `timeout` 配成 10 秒，并删掉外层包裹。如果指后者，则需要一个信号量：

```rust
pub struct BridgeOptions {
    pub timeout: Duration,
    pub max_in_flight: Option<usize>,   // None 表示无限
}
```

### 建议

先澄清再实现。若只是时长，kardia 已满足；若是并发数，约 40 行实现。

## K4 open 由宿主注入 client 标识（下游 P1）

### 下游主张

`open` 的 client 由客户端自报，不可信；应由宿主注入，例如 Tauri 的 webview label。

### 核对

`crates/kardia-bridge/src/protocol.rs`：

```rust
pub(crate) enum ClientRequest {
    Open {
        protocol: u32,
        client: String,
    },
    ...
}
```

下游描述准确：`client` 从请求体里读。

### 结论

成立，但这是 API 设计选择而非缺陷。kardia 的 crate 文档明确写了「宿主信任客户端」，在可信客户端的场景（同进程窗口）自报 client 名是合理简化。

要满足下游，需要一条宿主侧注入的路径。最小改动是新增方法而不是改签名：

```rust
impl Bridge {
    /// Handles one client request with the host-known identity of the client,
    /// which overrides the `client` field of an `open` request.
    pub async fn handle_as(
        &self,
        request: Value,
        client: &str,
        outbox: Option<Arc<dyn Outbox>>,
    ) -> Result<Value, BridgeError>
}
```

注意一个连带问题：`close_client(&str)` 按 client 名找会话。若宿主注入的名字与客户端自报的不同，关闭时必须用注入名，否则关不掉。实现里应统一约定「会话的 client 名一律以宿主注入的为准」。

工作量：`bridge.rs` 加 `session.rs` 约 50 行。

### 建议

采纳，作为 P1，用新增方法而非改签名。下游是唯一提出方，没有证据表明其它用户需要破坏性改动。

## K5 guest invoke 可指向宿主原生服务（下游 P2）

### 下游主张

`GuestHost::invoke` 只能调到 `GuestService`，宿主自己实现的服务只能以 JSON `view` 提供。

### 核对

`crates/kardia-hmr/src/guest/host.rs:72`：

```rust
/// Calls a [`GuestService`].
pub fn invoke(&self, key: &str, method: &str, args: Value) -> Result<Value> {
    self.ctx()?.get_as::<GuestService>(key)?.call(method, args)
}
```

下游描述准确：返回类型被钉死在 `GuestService` 上。

### 结论

成立。要支持宿主原生服务，需要一个方法表抽象：

```rust
pub trait HostAction: Send + Sync + 'static {
    fn call(&self, method: &str, args: Value) -> Result<Value>;
}
```

注册入口可以挂在 `GuestHost` 上，也可以作为服务 `provide_as` 到某个 key。为了避免与 `GuestService` 同名歧义，建议先用 `GuestService` 查，查不到再按 `HostAction` 查，并在错误里明确是哪一层没有。

工作量：`host.rs` 加 trait 与注册入口，约 120 行。

### 建议

可做，但不是本轮必需。下游自己也说「只有 K5 到 K9 全部具备后 guest 化才划算」，单独做 K5 收益有限。

## K6 单实例并发 export（下游 P2）

### 下游主张

一个实例同一时刻只跑一个 export，`process/instance.rs` 的 `exchange` 是串行消息回路。

### 核对

`crates/kardia-hmr/src/process/instance.rs:90`：

```rust
fn exchange(
    &mut self,
    export: Export,
    input: Vec<u8>,
    id: u64,
    deadline: Option<Instant>,
) -> Result<Result<Vec<u8>>, Fault>
```

`call(&mut self, ...)` 接收 `&mut self`，`exchange` 内部单循环逐条 `receive(deadline)`，并且对 `id` 不匹配的 host call 直接回 `"no such call"`。确实是串行回路。

### 结论

成立。要并发，需要把独占模型改成多路复用：

1. 消息带 `id`（`kardia-guest/2` 已有该字段）。
2. 拆分发送与接收分发：一个读循环把消息按 `id` 派发到各 export 的 oneshot。
3. host call 的反向请求也要带 `id` 并支持并发。

这是协议级改动，必须同步 Python SDK（`pykardia`）与所有 guest 侧实现。

工作量：XL（超过 800 行，跨语言）。

### 建议

本轮不做。下游自己也把它排在 P2 并标注工作量大。

## K7 host 到 guest 的 event（下游 P2）

### 下游主张

宿主动作只有 `provide`、`view`、`invoke`、`log`，没有 host 到 guest 的 `event`，也没有流式子消息。

### 核对

`guest/host.rs` 的公开方法：`provide`、`view`、`invoke`（另有内部 `logger`、`begin`、`finish`、`invoke_raw`）。没有 `event`。

### 结论

成立。但要注意：`event` 与 K6 的并发强耦合。一个脱离某个 export 的 host 到 guest 消息，在串行回路里只能在某个 export 挂起时被处理。因此 K7 必须在 K6 之后，否则事件会延迟到下一个 export 开始。

### 建议

与 K6 绑定，本轮不做。

## K8 guest 到 host 在 export 外主动调用（下游 P2）

### 下游主张

现在只有 `log` 能在 export 之外发起。

### 核对

`process/instance.rs` 的 `exchange` 循环里，对 `id` 不匹配的 host call 显式拒绝：

```rust
if v2 && message.get("id").and_then(Value::as_u64) != Some(id) {
    // a host call of no running export would wait forever
    if message.get("call").is_some() {
        self.shared.send(&json!({ "id": message.get("id"), "error": "no such call" }));
    }
```

下游描述准确：export 外的 host call 被拒绝。

### 结论

成立。同 K7，必须在 K6 之后才有意义。

### 建议

与 K6、K7 一起做，本轮不做。

## K9 ProcessGuest 重启、退避、崩溃循环（下游 P3）

### 下游主张

「进程退出时 `watch_exit` 让 fiber 失败，没有自动重启、退避、崩溃循环标记」。

### 核对

这条已经过时。kardia 0.3.0 已实现完整的 entry 级恢复策略。

`crates/kardia-core/src/recovery.rs`：

```rust
/// Failure isolation is independent of this policy. The default never retries.
#[derive(Clone, Default)]
pub enum RecoveryPolicy {
    #[default]
    Manual,
    Retry { delay: Duration },
    Host(Handler),
}
```

`crates/kardia-loader/src/recovery.rs` 提供配置读取与校验，配置形如：

```yaml
recovery:
  mode: retry
  delayMs: 1000
# 或 mode: host, handler: my-handler
# 或 mode: manual（默认）
```

`Fiber::set_recovery()` 安装监督任务（`supervise()`），fiber 进入 `Failed` 时按策略处理：

```rust
let action = match &policy {
    RecoveryPolicy::Manual => return,
    RecoveryPolicy::Retry { delay } if !error.is::<ValidationError>() =>
        Ok(RecoveryAction::RetryAfter(*delay)),
    RecoveryPolicy::Retry { .. } => Ok(RecoveryAction::KeepFailed),
    RecoveryPolicy::Host(handler) => { /* 宿主回调，panic 被隔离 */ }
};
```

`watch_exit()`（`crates/kardia-hmr/src/guest/bridge.rs:158`）在 guest 进程非正常退出时 `fiber.fail(exit.failure())`，这正是监督任务的触发点。因此「退出 → 失败 → 按策略重试」链路已经完整，并且默认策略是 `Manual`（不重试），符合仓库规则里「不强制自动恢复」的要求。

### 结论

不成立（文档过时）。下游文档写于 0.3.0 发布前，或未覆盖 `recovery` 配置。

但有两点仍成立：

| 主张 | 现状 |
| --- | --- |
| 崩溃循环标记 | 不具备。`supervise()` 没有「连续 N 次失败即停止」的熔断 |
| 退避 | 不具备。`Retry` 是固定延迟，不是指数退避 |

### 建议

1. 下游修正文档里 K9 的描述：重启与固定延迟重试已具备，仅缺熔断与指数退避。
2. 若要补，应在 `RecoveryConfig::Retry` 上增加 `maxRetries` 与 `backoff` 字段，而不是在进程层另做一套。工作量 M。

## 汇总

| 编号 | 下游定性 | 核对结果 | kardia 应对 | 优先级 |
| --- | --- | --- | --- | --- |
| K1 | P0 成立 | 准确 | 采纳，`Error` 带 `FiberId` | P0 |
| K2 | P1 成立 | 准确 | 采纳，加 `emit_scoped` 与 `SessionView` | P1 |
| K3 | P1 成立 | 部分不成立：`timeout` 已是单次调用上限 | 先澄清时长还是并发 | P1 |
| K4 | P1 成立 | 准确，属设计选择 | 采纳，新增 `handle_as` | P1 |
| K5 | P2 成立 | 准确 | 可做，非必需 | P2 |
| K6 | P2 成立 | 准确 | 协议级改动，XL | 暂缓 |
| K7 | P2 成立 | 准确，且依赖 K6 | 与 K6 绑定 | 暂缓 |
| K8 | P2 成立 | 准确，且依赖 K6 | 与 K6 绑定 | 暂缓 |
| K9 | P3 成立 | 不成立（过时），仅缺熔断与指数退避 | 下游更新文档；kardia 视需补 `maxRetries` | P3 |

## 下游文档需要修正的地方

| 位置 | 现有说法 | 应改为 |
| --- | --- | --- |
| 1.3 表格「重启」行 | 「没有自动重启、退避、崩溃循环标记」 | 「已有 manual、retry、host 三种 entry 级恢复策略，默认 manual；缺指数退避与崩溃循环熔断」 |
| 1.3 表格「并发」行 | 「`try_lock_until` 等待而非直接拒绝」 | 补充「仍建立在 `&mut self` 的串行回路上」 |
| 1.2 原文 | 「`kardia-bridge` 本身不设上限」 | 「仅有 `BridgeOptions.timeout` 单次调用超时，没有在途并发上限」 |
| 第 3 节 K9 | 「`ProcessGuest` 重启、退避、崩溃循环」 | 拆分：重启与固定延迟已具备；仅需 `maxRetries` 与退避曲线 |
| 第 4 节 | 「代价是两套生命周期模型长期并存」 | 仍成立，可补充「entry 级 recovery 已能覆盖 guest 崩溃后的恢复」 |

## 与上游 Cordis 的关系

这九条诉求与 Cordis 上游本轮 37 个提交完全无关（见 `01-upstream-overview.md` 第 4 节）。原因很直接：

- `kardia-bridge` 与 `kardia-guest` 是 kardia 独有的 crate，上游 Cordis 没有对应物。
- K1（事件带 `FiberId`）在上游也没有。Cordis 的 JS 事件模型没有 fiber 归属概念，panic 直接进 logger。

因此下游反馈必须由 kardia 自己排期，不能指望「跟进上游」顺带解决。

## 待确认项

以下三点需要与下游确认后才能定实现，不宜自行假设：

1. K3 的「上限」指时长还是并发数。若指时长，kardia 已有，下游应删外层；若指并发，需要信号量。
2. K2 的 owner 隔离是「桥提供过滤能力」还是「桥自己知道 owner」。前者只给谓词；后者需要会话元数据机制。
3. K9 是否只需要 `maxRetries`，还是要完整的指数退避加抖动。

## 核对命令

```bash
cd /root/code_workspace/kardia
grep -n "from_panic" -A 10 crates/kardia-core/src/error.rs
grep -n "fn emit\|fn broadcast\|fn send_all" crates/kardia-bridge/src/bridge.rs
grep -n "pub struct BridgeOptions" -A 6 crates/kardia-bridge/src/bridge.rs
grep -n "enum ClientRequest" -A 6 crates/kardia-bridge/src/protocol.rs
grep -n "pub fn invoke" -A 3 crates/kardia-hmr/src/guest/host.rs
grep -n "enum RecoveryPolicy" -A 8 crates/kardia-core/src/recovery.rs
```
