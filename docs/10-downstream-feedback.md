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

### 核对（跟进前）

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

成立。要并发，需要把独占模型改成多路复用：消息带 `id`；一个读循环按 `id` 派发到各 export 的等待者；host call 的反向请求带 `seq` 并支持并发。这是协议级改动，必须同步 Python SDK（`pykardia`）与所有 guest 侧实现。工作量 XL。

### 已跟进：`kardia-guest/3`

**握手**。host 以 `{ export: "hello", input: { protocol: "kardia-guest/2" } }` 开场——这是 0.2 版 guest 唯一认识的名字——guest 回答**它能说的最高版本**：`{ ready: "kardia-guest/3" }` 或 `{ ready: "kardia-guest/2" }`，host 按回答服务；回 `{ done: 1 }` 的是版本 1 guest。所以用 kardia-guest 0.2 构建的 guest 在新 host 上照常运行；反过来，新 SDK 构建的 guest 不能在只认版本 2 的旧 host 上运行（旧 host 对 `ready: kardia-guest/3` 报未知协议）。曾尝试「先提供 v3、失败再提供 v2」的两次握手，不可行：0.2 guest 把第一个 hello 当未知 export 后就进入版本 1 回路，不再处理第二个 hello。

版本 3 之下：

| 方向 | 消息 | 说明 |
| --- | --- | --- |
| host→guest | `{ id, export, input }` | 同前，但**多条可同时在途** |
| guest→host | `{ seq, call, key, …, id? }` | `seq` 由 guest 分配，回复 `{ seq, result/error }`；host 在读线程上同步处理，多条 host call 可交叉在途，且**不要求属于任何 export** |
| host→guest | `{ event: name, payload }` | K7，随时可发 |
| host→guest | `{ cancel: id }` / `{ ping: n }` | 同版本 2 |

**host 侧**（`process/instance.rs`）。`ProcessInstance::call` 取 `&self`；一个读线程独占 `incoming`，把 `output`/`done` 送到对应 `id` 的等待者，host call 在读线程上直接执行并按 `seq` 回写，`Closed` 广播给所有等待者。`bridge::run` 对 `Export::Call` 不再持 `Slot.instance` 的锁（只 clone 出 `Arc<dyn GuestInstance>`），`meta`/`apply`/`dispose` 仍独占。WASM 与 dylib 后端不多路复用，用 `Serial<T>` 把自身包在锁后，行为与原来一致。

**guest 侧**（`kardia-guest/src/serve.rs`）。读线程处理 ping、cancel、host call 回复、event；每个 `call` export 在自己的线程上跑，`meta`/`apply`/`dispose` 等在途 `call` 归零后独占执行。

**重入**。原来靠 thread-local `ENTERED` 拒绝「guest 调自己」，因为 host call 在调用线程上同步处理。现在 host call 在读线程上处理，`ENTERED` 失效；改为 `Slot` 上的 `answering` 计数：`GuestHost::invoke` 期间 +1，`bridge::run` 入口看到目标 slot 的 `answering > 0` 即拒绝（间接重入 A→B→A 同样覆盖）。检查必须放在拿 `Slot.instance` 锁**之前**，否则 lifecycle export 持锁期间会死锁——这是实现时真实踩到的坑，表现为 WASM 的重入测试挂死。

**验证**。`crates/kardia-hmr/tests/process.rs`：`calls_of_one_instance_run_at_the_same_time`（两个 400 ms 的 `sleep` 并发，总耗时 < 750 ms）、`serves_a_guest_speaking_kardia_guest_2`（手写 v2 fixture：握手降级、ping 可用、`emit` 被拒）；`crates/kardia-guest/tests/serve.rs`：握手、`seq` 回复、cancel。

## K7 host 到 guest 的 event（下游 P2）

### 下游主张

宿主动作只有 `provide`、`view`、`invoke`、`log`，没有 host 到 guest 的 `event`，也没有流式子消息。

### 核对（跟进前）

`guest/host.rs` 的公开方法：`provide`、`view`、`invoke`（另有内部 `logger`、`begin`、`finish`、`invoke_raw`）。没有 `event`。

### 结论

成立。`event` 与 K6 的并发强耦合：一个脱离某个 export 的 host 到 guest 消息，在串行回路里只能在某个 export 挂起时被处理。因此在 K6 之后做。

### 已跟进

`GuestService::emit(event, payload)` → `GuestInstance::emit` → `{ event, payload }`；guest 侧读线程收下后立即交给 `Guest::event(&self, host, name, payload)`，**不排在 export 队列后**，与正在跑的 `call` 并发（这也是 `Guest::event` 取 `&self` 的原因）。事件自带的 `host` 是「无 export 的 host」，guest 在回调里通过它调 host 就是 K8。

WASM 与 dylib 没有到运行中 guest 的通道，`emit` 在那里返回错误。

**验证**。`process.rs::an_event_reaches_the_guest_between_exports`：先发一条、再在 `sleep` 进行中发一条，两条都到达且各自记录了当时 host 的 view。

## K8 guest 到 host 在 export 外主动调用（下游 P2）

### 下游主张

现在只有 `log` 能在 export 之外发起。

### 核对（跟进前）

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

成立。同 K7，在 K6 之后才有意义。

### 已跟进

`Host::detached()` 返回一个可在 `apply` 之后一直保留的句柄；在版本 3 的进程 guest 上它从任何线程、任何时刻都能 `view`/`invoke`/`provide`/`log`——消息带 guest 分配的 `seq`，host 读线程按 `seq` 回复。WASM、dylib 与版本 1 进程 guest 没有这条通道，detached host 上的调用返回错误（而不是静默丢失）。event 回调里拿到的 host 也是 detached 的。

**验证**。`process.rs::a_guest_calls_the_host_from_outside_its_export`；`serve.rs` 里 `detach` 方法用 `apply` 时存下的 host `view` 自己。

### 遗留：guest 侧并发对作者的要求

`Guest::call` 从 `&mut self` 改为 `&self`，是这次唯一的 SDK API 破坏性变更。原因是并发 export 需要同时借用实例；代价是 guest 作者要把 `call` 与 `event` 共享的状态放到 `Mutex`/原子后面（fixture 的 `calls`/`cancelled` 改成了 `AtomicU64`）。WASM 与 dylib 后端虽然不会并发调用，但 trait 签名统一，所以同样受影响。

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

| 编号 | 下游定性 | 核对结果 | kardia 应对 | 状态 |
| --- | --- | --- | --- | --- |
| K1 | P0 成立 | 准确 | `Error` 带 `FiberId`；`Context::fiber_of(FiberId) -> Option<Fiber>` | 已做（`eaedf32`、`f1cd38b`） |
| K2 | P1 成立 | 准确 | `emit_scoped` + `SessionView{id, client, owner}`；`handle_with(request, client, owner, outbox)`；回执含 `owner` | 已做（`8ff4d17`、`f1cd38b`） |
| K3 | P1 成立 | 部分不成立：`timeout` 已是单次调用上限 | `BridgeOptions.max_in_flight`（信号量，`None` 不限） | 已做（`f1cd38b`） |
| K4 | P1 成立 | 准确，属设计选择 | `handle_as` | 已做（`8ff4d17`） |
| K5 | P2 成立 | 准确 | `HostAction` trait + `register_action`；`GuestHost::invoke` 先查 `GuestService` 再回退 `Registered` | 已做（`d90304b`） |
| K6 | P2 成立 | 准确 | `kardia-guest/3`：多路复用，单实例 `call` 并发 | 已做（见下） |
| K7 | P2 成立 | 准确，且依赖 K6 | `GuestService::emit` → `Guest::event(&self, …)` | 已做（见下） |
| K8 | P2 成立 | 准确，且依赖 K6 | `Host::detached()`，export 外任意线程调 host | 已做（见下） |
| K9 | P3 成立 | 不成立（过时），仅缺熔断与指数退避 | `Retry { delay, max_retries, backoff }`，`Backoff::{Fixed, Exponential}` | 已做（`27ecc9a`） |

K1–K9 至此**全部落地**。

## 下游文档需要修正的地方

| 位置 | 现有说法 | 应改为 |
| --- | --- | --- |
| 1.3 表格「重启」行 | 「没有自动重启、退避、崩溃循环标记」 | 「已有 manual、retry、host 三种 entry 级恢复策略，默认 manual；retry 支持 `maxRetries` 与指数退避（6 步封顶）」 |
| 1.3 表格「并发」行 | 「`try_lock_until` 等待而非直接拒绝」 | 「`kardia-guest/3` 进程 guest 的 `call` 在单实例内并发；lifecycle export 仍串行」 |
| 1.2 原文 | 「`kardia-bridge` 本身不设上限」 | 「`BridgeOptions.max_in_flight` 限制在途请求数，`timeout` 限制单次时长」 |
| 第 3 节 K9 | 「`ProcessGuest` 重启、退避、崩溃循环」 | 重启、固定/指数退避、`maxRetries` 均已具备 |
| K6 表格行 | 「可能破坏：去掉自建的并发上限」 | 成立：guest 侧 `Guest::call` 改 `&self`，共享状态需加锁 |
| K7 表格行 | 「兼容（新增）」 | 对 WASM/dylib 不适用：无通道，`emit` 返回错误 |
| K8 表格行 | 「兼容（新增）」 | 对 WASM/dylib 不适用：detached host 上调用返回错误 |

## 与上游 Cordis 的关系

这九条诉求与 Cordis 上游本轮 37 个提交完全无关（见 `01-upstream-overview.md` 第 4 节）。原因很直接：

- `kardia-bridge` 与 `kardia-guest` 是 kardia 独有的 crate，上游 Cordis 没有对应物。
- K1（事件带 `FiberId`）在上游也没有。Cordis 的 JS 事件模型没有 fiber 归属概念，panic 直接进 logger。

因此下游反馈必须由 kardia 自己排期，不能指望「跟进上游」顺带解决。

## 核对命令

```bash
cd /root/code_workspace/kardia
grep -n "pub fn fiber_of" crates/kardia-core/src/context.rs
grep -n "pub fn emit_scoped\|pub fn handle_with\|pub fn handle_as\|max_in_flight" crates/kardia-bridge/src/bridge.rs
grep -n "pub trait HostAction" crates/kardia-hmr/src/guest/action.rs
grep -n "pub fn emit" crates/kardia-hmr/src/guest/service.rs crates/kardia-hmr/src/guest/host.rs
grep -n "pub fn detached\|fn event" crates/kardia-guest/src/host.rs crates/kardia-guest/src/guest.rs
grep -n "pub(crate) const PROTOCOL\|fn exchange\|fn spawn_reader" crates/kardia-hmr/src/process/instance.rs
grep -n "enum RecoveryPolicy" -A 8 crates/kardia-core/src/recovery.rs
cargo test -p kardia-hmr --test process
cargo test -p kardia-guest --test serve
```

## 现状复盘点（2026-11-29）

上文的评估写于 K1–K9 尚未落地时。以下按**当前代码**逐条列出验证点与对应测试。

### 全部已落地

| 编号 | 验证点 | 位置 |
| --- | --- | --- |
| K1 | `Context::fiber_of(FiberId) -> Option<Fiber>` | `crates/kardia-core/src/context.rs:62` |
| K2 | `handle_with(request, client, owner, outbox)`；`Session.owner`；`SessionView.owner` | `crates/kardia-bridge/src/bridge.rs:21,32,327` |
| K3 | `BridgeOptions.max_in_flight: Option<usize>` → `Session.slots: Option<Arc<Semaphore>>` | `bridge.rs:32` |
| K4 | `Bridge::handle_as` | `bridge.rs` |
| K5 | `HostAction` + `register_action` | `crates/kardia-hmr/src/guest/action.rs:14` |
| K6 | `ProcessInstance::call(&self)`；读线程按 `id` 派发；`bridge::run` 的 `Call` 分支不持 `Slot.instance` 锁 | `crates/kardia-hmr/src/process/instance.rs`、`guest/bridge.rs` |
| K7 | `GuestService::emit` → `GuestInstance::emit` → `{ event, payload }` → `Guest::event(&self)` | `guest/service.rs`、`guest/host.rs`、`crates/kardia-guest/src/serve.rs` |
| K8 | `Host::detached()`；host call 带 `seq`，读线程按 `seq` 回复，不要求属于 export | `crates/kardia-guest/src/host.rs`、`serve.rs`、`process/instance.rs` |
| K9 | `Retry { delay, max_retries, backoff }` | `crates/kardia-core/src/recovery.rs:38` |

### 协议与兼容边界

| 项 | 现状 |
| --- | --- |
| 握手 | host 发 `protocol: "kardia-guest/2"`，guest 回答自己能说的最高版本（`ready: kardia-guest/3` 或 `kardia-guest/2`） |
| 旧 guest → 新 host | kardia-guest 0.2.x 构建的 guest 按版本 2 串行运行，ping/cancel 可用，`emit` 被拒；`tests/hmr/test_process_guest.py` 与 kardia 的 `serves_a_guest_speaking_kardia_guest_2` 覆盖 |
| 新 guest → 旧 host | **不兼容**：0.2.x host 对 `ready: kardia-guest/3` 报未知协议 |
| `Guest::call` 签名 | `&mut self` → `&self`，`Guest: Send + Sync`；所有后端（含 WASM/dylib）统一 |
| WASM / dylib | 不多路复用，`Serial<T>` 串行；无 `emit`、无 detached 通道 |
| `pykardia` | host 同时支持 v2/v3 guest；guest SDK 只说 v3；`tests/parity/rust_guest/` 的 0.2.2 固定副本仍作为 v2 验收 |

### 实现时踩到的两个坑

1. **lifecycle export 持锁期间重入会死锁**。`bridge::run` 的 lifecycle 分支持有 `Slot.instance` 锁时，guest 调 `host.invoke("自己")` → `GuestService::call_with` → `slot.running()` 要拿同一把锁。表现为 WASM 的 `rejects_calls_back_into_a_running_guest` 挂死。修法：重入检查（`Slot::reentrant()`）放在拿锁之前。
2. **两次握手不可行**。「先提供 v3、guest 回 `done:1` 再提供 v2」看起来合理，但 0.2 guest 把第一个 hello 当未知 export 后已进入版本 1 回路，不再处理第二个 hello。改为一次握手、guest 自报版本。

### 验证范围

- `cargo test --workspace`：98 个目标全绿；其中 `kardia-hmr --test process` 17 项（并发、event、detached、v1、v2、deadline、cancel、ping、frame limit、exit、IO panic）、`kardia-guest --test serve` 3 项。
- `cargo fmt --check`、`cargo clippy -D warnings`、`cargo xtask arch`：通过。
- `pykardia`：`unittest discover` 540 项全过（13 skipped），`ruff check/format`、`ty check` 通过。
