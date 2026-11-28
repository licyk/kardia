# 下游恢复需求：分析与实现计划

本文对 `docs/recovery-requests.md`（hanafubuki-agent 提出的 R1–R4）逐条核对
当前 `kardia-core` 0.4.0 的实现，给出缺口结论和实施计划。

## 一、需求背景与核对方法

下游 hanafubuki-agent 用 `kardia-core` 0.4.0（`Cargo.toml` 固定 `kardia-core =
"0.4.0"`）托管插件行，目前仍自带一套重启实现：

| 下游实现 | 位置 | 作用 |
|---|---|---|
| `RestartPolicy` / `RestartWindow` | `crates/host/src/restart.rs` | 延迟表 2s/10s/30s，10 分钟窗口，窗口内用完后保持失败，用户重试清空窗口 |
| `RestartRow` 调度 | `crates/host/src/host/restarts.rs` | `schedule_restart` 起 tokio 任务；`PendingRestart::cancel` 用 `task.abort()` 取消 |
| 界面字段 | `crates/host/src/status.rs` | `restarts_in_window`、`next_restart_at` |

实际检索：下游 `src-tauri` 与 `crates/host` 中**没有任何** `set_recovery` /
`RecoveryPolicy` 调用，R1–R4 的缺口成立，需求是真实且未被满足的。

## 二、逐条核对结果

### R1 策略需要知道失败发生在哪个阶段 —— 缺口成立

`Failure`（`crates/kardia-core/src/recovery.rs:22`）只有 `fiber` 和 `error`。

而 `kardia-core` 内部确实区分了这两种失败：

- **启动失败**：`lifecycle.rs:145` 的 `finish_reload` 在 `apply` 失败时写入
  `fiber.error`；
- **运行中故障**：`lifecycle.rs:172` 的 `fail()` 由 `Fiber::fail` 触发。

两者都收敛成 `FiberState::Failed` + `error`，`Failed` 之后
`state.inertia == false`。所以外部**无法**区分"构造器失败"与"运行中崩溃"，
下游只能靠自己不复用同一实例来规避。

顺带确认：`supervise()` 重启走的是 `fiber.update_with(fiber.config(), true)`
（`recovery.rs:212`），即重新执行 `apply`。构造器若失败，每次重试都会同样失败，
这与下游"启动失败不自动重启"的要求直接冲突，正是 R1 要解决的问题。

### R2 需要窗口而不是仅计数 —— 缺口成立

`RecoveryPolicy::Retry` 的 `max_retries` 在 `supervise()` 中按
`attempts >= limit` 判定（`recovery.rs:168`），`attempts` 是 supervisor 生命周期内
的累计值，没有时间维度。下游要求的是"窗口内 N 次"，窗口滑过后计数应重置。

数学上可以确认：`Retry { delay: 2s, max_retries: Some(3), backoff: Exponential }`
恰好得到 2s/4s/8s 三次，但**第四次之后永久失败**，不满足"一天崩一次不该
第四次就永久失败"。

### R3 需要看到并取消待执行的重试 —— 缺口成立，且最重

- `RecoveryTask`（`recovery.rs:76`）只有 `Drop` 里的 `abort()`，字段
  `AbortHandle` 私有，`Fiber` 上没有读取入口；
- `Fiber` 的公开方法（`fiber.rs:51-196`）里没有 `retry_now`、
  `pending_retry`、`attempts`；
- `events.rs` 没有恢复相关事件，`StatusChanged` 不会告知"已安排在 X 秒后重启"。

因此下游界面上的 `next_restart_at` 只能来自它自己的 `PendingRestart`。

### R4 释放必须取消待执行的重试 —— ⚠️ 需实测确认，文档称"已满足"

发现一处可能的**真实缺陷**：`supervise()` 在等待延迟时用的是

```rust
tokio::select! {
    _ = tokio::time::sleep(delay) => {}
    _ = fiber.ctx.root.wait_until(|st| st.fiber(fiber.id()).is_none_or(|state| {
        state.removed || state.revision != revision || state.error...
    })) => continue,
}
```

`wait_until`（`root.rs:68`）先求值谓词再等待，且谓词对**不存在的 fiber** 返回
`true`（`is_none_or`）。因此 `collect_removed` 把 fiber 从 `self.fibers` 移除后
该分支会立即唤醒，走 `continue`，下一轮 `fiber.is_removed()` 返回 true 进而
`return`。这条路径是正确的，且 `set_recovery` 重新设置时也会取消旧任务
（`recovery.rs:97-100` 的 `fiber.recovery.take()` + `drop_later`）。

但有两个未被测试覆盖的空隙：

1. **重试窗口之外**：`update_with` 已经提交、`settle.await` 进行中的时刻，
   若此刻发生 dispose，supervisor 是否会在卸载后仍触发下一次重启；
2. **`RecoveryTask::drop` 是否一定被执行**：`st.drop_later(old)` 在 root
   关闭时的执行时序。

结论：R4 不是"要加新功能"，而是**要补一个覆盖 disposal × pending-retry ×
重复失败循环的实测证据**。若实测暴露问题，则按其严重程度修复。

## 三、需求映射到 API 的设计

四条需求合并起来，本质是"策略可见 + 可观测 + 可取消"。设计如下（破坏性变更，
仓库规则允许，且 0.4.0 刚发布，可走 0.5.0）：

### 3.1 失败阶段（R1）

```rust
pub enum Phase { Start, Running, Stop, Disposing }

pub struct Failure {
    pub fiber: Fiber,
    pub error: Error,
    pub phase: Phase,
    /// 本次失败前已自动重试的次数
    pub attempts: u32,
}
```

`lifecycle` 在写入 `fiber.error` 时同时写入 `phase`（`finish_reload` →
`Start`，`fail()` → `Running`）。`Fiber::fail` 从插件内部调用时也是 `Running`。

### 3.2 延迟表（R2）

新增：

```rust
RecoveryPolicy::retry_delays(&[Duration]) // 第 n 次等 delays[n]；表尾即上限
```

或保留 `Retry` 并增加 `window: Option<Duration>`。**取舍**：延迟表更贴近下游
现有语义（2/10/30 秒表尾即停），窗口则解决"一天崩一次"的累积问题。计划两者都
要：`delays` 决定节奏，`window` 决定计数何时重置（`None` = 不重置，即现在的
行为）。

### 3.3 可观测与即时重试（R3）

```rust
pub struct RecoveryState {
    pub attempts: u32,
    pub pending: Option<PendingRetry>, // { due_at: Instant, attempt: u32 }
}

impl Fiber {
    pub fn recovery_state(&self) -> RecoveryState;
    pub fn retry_now(&self) -> Result<Settle>;  // 取消待执行并立即重挂
}
```

外加事件 `RecoveryScheduled { fiber, due_at, attempt }`，让下游不必轮询（它已经
在监听 `StatusChanged`）。

实现要点：`RecoveryTask` 内需要共享一个 `Arc<Mutex<Option<PendingRetry>>>`，
supervisor 在 sleep 前写入、醒来后清空；`retry_now` 通过一个 `Notify` 或
直接 `abort` + `update_with` 完成。

### 3.4 取消（R4）

不新增 API；补测试证明 `dispose`、`set_recovery(Manual)`、`update` 三种情况下
待执行重试都被取消。若实测失败则修复。

## 四、实施计划

按依赖顺序分四步，每步可独立提交、独立验证：

### 第 1 步：R1 失败阶段（最小、最先）

1. `state.rs` 的 `FiberInner` 增加 `phase: Option<Phase>`（与 `error` 同寿命）；
2. `lifecycle.rs` 的 `finish_reload` / `fail` 写入 phase；
3. `Failure` 增加 `phase` 字段，`supervise` 填充；
4. 测试：构造器失败 → `Phase::Start`；运行中 `fiber.fail()` → `Phase::Running`；
5. 下游据此实现"Start 不自动重试"。

### 第 2 步：R2 延迟表 + 窗口

1. `RecoveryPolicy::Retry` 增加 `delays: Option<Vec<Duration>>` 与 `window: Option<Duration>`
   （或用新的构造器 `retry_delays`，保持破坏性清晰）；
2. `supervise` 用"窗口内 attempt 计数"替代累计 `attempts`；
3. 测试：表尾停止、窗口滑过后计数重置、一天崩一次不会永久失败。

### 第 3 步：R3 可观测 + retry_now

1. `RecoveryTask` 携带 `Arc<Mutex<RecoveryState>>`；
2. `Fiber::recovery_state()` / `Fiber::retry_now()`；
3. 新事件 `RecoveryScheduled`；
4. 测试：pending 的 `due_at` 可读、`retry_now` 立即执行且不等待延迟、
   事件在安排时发出。

### 第 4 步：R4 取消的正确性证明

1. 测试矩阵：dispose / set_recovery(Manual) / update / root 关闭 × pending 重试；
2. 重复失败—恢复循环若干次，验证无重复注册、无泄漏的 supervisor；
3. 若暴露缺陷则修复（例如在 `update_with` 前再检查一次 `is_removed`）。

### 第 5 步：文档与下游验收

1. 更新 `site/content/docs/guide-server.mdx`（中英）的 recovery 段落，
   写明 `phase`、延迟表、`recovery_state`、`retry_now`；
2. 在 hanafubuki-agent 侧用真实故障注入验收：删除 `RestartWindow`/
   `RestartPolicy` 与自带调度，改用 kardia 的策略，验证：
   - 自动重试在无测试驱动重启的情况下恢复；
   - 手动模式保持失败直到显式处理；
   - 宿主自定模式按宿主决策执行且处理器错误被隔离；
   - 处置取消待执行重试；
   - 失败与恢复过程中无关插件继续工作。
3. 版本：`kardia-core` 及依赖它的包升 0.5.0（`Failure` 与 `RecoveryPolicy`
   的破坏性变更），用 `cargo xtask release minor` 发布。

## 五、风险与取舍

- **破坏性变更**：`Failure` 增加字段会让下游 `RecoveryPolicy::host` 的闭包
  签名不变但需要解构新字段，属于编译期可发现的破坏，可接受。
- **`retry_now` 与 supervisor 的竞争**：实现必须让 `retry_now` 先取消
  supervisor 的待执行状态，再触发 `update_with`，否则可能出现两次重挂。
  这是本计划唯一需要仔细处理并发的地方。
- **窗口语义**：`window: None` 保持现有行为，避免让现有用户意外改变语义。
- **不请求的部分**（保持不做）：根 fiber 不自动重试、`ValidationError` 不重试、
  不引入配置格式。
