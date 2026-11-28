# 03 loader：entry 行为、转移与自弃用

涉及提交：`915e17a`（行为下移到 options）、`8dbd5d3`（转移后的释放归属）、`279ac22`/`27b9010` 的 loader 侧改动，以及 `include` 的 `ctx.entry` 替换。

改动文件：`packages/loader/src/**` 13 个文件 +343/−223；`packages/include/src/**` 同步。

## 1. 从「全局事件监听」到「per-fiber 选项」

### 1.1 删除的东西

`packages/loader/src/index.ts` 在基线上有 116 行监听逻辑，跟进后全部删除，`Loader` 类只剩树建构与 `provide`：

```ts
// 基线
const sources = new WeakMap<Fiber, any>()
ctx.on('internal/config', (fiber, next) => { ... }, { prepend: true })   // 校验追踪
ctx.on('internal/config', (fiber, next) => { ... })                       // 插值
ctx.on('internal/commit', (fiber, config) => { ... })                     // 回写
ctx.on('internal/update', function (config, next) { ... }, { global: true })
ctx.on('internal/status', (fiber) => { ... })                             // 清 verdict
ctx.on('internal/plugin', (fiber) => { ... })                             // 自弃用
```

### 1.2 新位置

每个钩子搬进 `Entry._start()` / `Entry._init()` 传给 `registry.use()` 的 `Fiber.Options`：

```ts
this.innerFiber = innerContext.registry.use(component, this.options.config, {
  getStack: this.getStack,
  config(config, resolve) {
    if (this.runtime!.callback[EntryGroup.key]) return resolve(config)
    return resolve(interpolate(this.ctx, config, entry.trace, ['config']))
  },
  commit(config) {
    const unparse = this.runtime?.Config?.['simplify']
    const legacy = { ...entry.options }
    entry.options.config = unparse ? unparse(config) : config
    entry.parent.tree.commit({ id: entry.options.id, group: entry.parent, options: entry.options, legacy })
  },
  dispose() {
    entry._release(this)
    entry._onDispose(this)
  },
})
```

outer fiber（承载 `inject` 的那层）也有自己的 `dispose() { entry._release(this) }`。

### 1.3 `ctx.entry` 消失

基线用 `interface Fiber { entry?: Entry }` 加 context 原型链继承，让嵌套插件能读到 `ctx.fiber.entry`。跟进后改为在 loader 的 Context 上挂一个唯一 symbol：

```ts
export class Entry {
  static readonly key: unique symbol = Symbol.for('cordis.entry')
}

declare module 'cordis' {
  interface Context {
    loader: Loader
    [Entry.key]?: Entry
  }
}
```

所有读取点统一改成 `ctx[Entry.key]`：

| 位置 | 基线 | 跟进后 |
| --- | --- | --- |
| `Entry.id` | `this.parent.tree.ctx.fiber.entry` | `this.parent.tree.ctx[Entry.key]` |
| `_parentDisabled()` | `this.parent.ctx.fiber.entry` | `this.parent.ctx[Entry.key]` |
| `EntryGroup` 构造 | `ctx.fiber.entry` | `ctx[Entry.key]` |
| `EntryTree` 构造 | `this.ctx.fiber.entry` | `this.ctx[Entry.key]` |
| `include` 的 `enableLogs`、`commit`、`trace`、写回 | `ctx.fiber.entry` | `ctx[Entry.key]` |

关键点：`Entry` 构造时把 symbol 放进 `ctx.extend({ [Entry.key]: this })`，**同时**给 `this.ctx` 建了独立的 isolate 表（`Object.create(this.ctx[Context.isolate])`）。因此嵌套插件通过原型链仍能读到 entry，但这次是显式符号而不是附加字段。

## 2. 转移（transfer）与持有者（holder）

### 2.1 问题

一个 entry 从 group A 移到 group B（配置文件里改写或者跨文件）。基线只把 fiber 挪过去，但 fiber 的 disposer 仍注册在**创建它的** fiber（A 的 fiber）上。结果：A 被卸载时会把已经属于 B 的 entry 一起拆掉。

### 2.2 修复

新增 `Entry._hold(fiber)`：

```ts
private _hold(fiber: Fiber) {
  const owner = this.ctx.fiber          // 当前持有它的 group fiber
  const previous = this._holder
  if (previous?.owner === owner) return
  // 正在卸载的 group 不能接管；在它停下之前，原 group 保留注册
  if (owner.uid === null) return
  const holder = { fiber, owner, release: undefined! as () => void }
  holder.release = owner.effect(() => () => {
    if (this._holder === holder) return fiber.dispose()
  }, 'entry')
  if (previous) {
    previous.owner.leak(previous.release)
  } else {
    owner.leak(fiber.dispose)
  }
  this._holder = holder
}
```

三个要点：

1. **先注册新持有者，再解绑旧持有者**。中间不会出现无人持有的窗口。
2. `previous.owner.leak(previous.release)` 与 `owner.leak(fiber.dispose)` 用 `Fiber.leak()` 把 disposer 从旧 fiber 上摘掉。这就是 `02` 里 `leak()` 的唯一实际用途。
3. `owner.uid === null` 时直接返回——正在卸载的 group 拒绝接管，避免注册到马上要清空的列表上。

配套的 `_release(fiber)` 在处理「组件自己 dispose 自己」时把持有者关系摘掉，让 group 不再负责它。

### 2.3 调用点

```ts
if (this._hasOuter()) {
  this.outerFiber = this.ctx.registry.use(..., { getStack, dispose() { entry._release(this) } })
  this._hold(this.outerFiber)
} else {
  this._start(this.ctx, exports.component)
}
// _start() 里：
if (ctx === this.ctx) this._hold(this.innerFiber)
```

`ctx === this.ctx` 这个条件区分「直接在 entry 的 ctx 上启动」与「在 outer fiber 的 ctx 上启动」：后者已经由 outer fiber 持有，不需要 inner fiber 再持有一遍。

`Entry.update()` 里也补了一句 `if (this._holder) this._hold(this._holder.fiber)`，在 context 被重新挂到新 parent 之后重建持有关系。

## 3. 自弃用（self-dispose）判定的四条分支

`_onDispose()` 现在是显式回调，注释写清了四种情况：

```ts
private _onDispose(fiber: Fiber) {
  // 1. entry 所在的整棵树正在销毁
  if (!this.parent.tree.ctx.fiber.uid) return
  this.loader.showLog(this, 'unload')
  // 2. 父级被卸载，连带把 entry 一起带走
  //    （父级可能仍处于注册状态，例如它自己掉了依赖）
  if (fiber.parent.fiber.state === FiberState.UNLOADING) return
  // 3. entry 被 loader 移除（EntryGroup.remove 先 unregister 再 dispose）
  if (!this._registered()) return
  // 4. 通过 entry.dispose() 主动处理（配置文件更新、祖先 group 被禁用、rebuild）
  if (this._disposing) return
  // 其余情况：组件自己把自己 dispose 了 → 落盘 disabled: true
  const legacy = { ...this.options }
  this.options.disabled = true
  this._verdict = true
  this.parent.tree.commit({ id: ..., group: this.parent, options: this.options, legacy })
  if (this.outerFiber) this.dispose()
}
```

与基线相比有两处实质变化：

- 基线靠 `fiber.entry` 与 `fiber.parent.fiber?.entry` 的相等性排除子插件。现在只有 entry 的根 fiber 才带 `dispose` 选项，天然排除。
- 基线在 `internal/plugin` 里先判断 `if (fiber.uid) return`（fiber 被创建）。现在这个判断消失——`dispose` 只在卸载时被调用。

## 4. verdict 与依赖丢失

`disabled` 表达式的求值结果存在 `Entry._verdict`。它是在**outer fiber 的服务环境下**求出来的，因此 outer fiber 一旦开始卸载（典型原因：依赖丢失），这个结论就失效：

```ts
private _enabled() {
  return this._verdict === false && this.outerFiber?.state !== FiberState.UNLOADING
}
```

外层 `outer` 插件的 `yield` 回调在卸载时清掉 verdict：

```ts
const outer = {
  async* apply(ctx: Context) {
    const entry = ctx[Entry.key]!
    const exports = await entry._import()
    yield () => {
      if (entry.outerFiber?.ctx.fiber === ctx.fiber) entry._verdict = undefined
    }
    if (exports) entry._start(ctx, exports.component)
  },
}
```

基线用 `internal/status` 事件 + `entry.outerFiber?.uid === fiber.uid` 达到同样效果；现在 `yield` 是 effect 的逆操作，生命周期天然正确。

## 5. 日志归属

`Entry.update()` 里新增判断：承载 entry 的组件（group、include）不再自己打 `reload` 日志，交给它的子 entry：

```ts
if (diff.includes('config') || this.options.group) {
  const { prototype } = this.innerFiber!.runtime!.callback
  if (!(prototype instanceof EntryGroup || prototype instanceof EntryTree)) {
    this.loader.showLog(this, 'reload')
  }
  await this.innerFiber!.update(this.options.config, true)
}
```

原因：之前 group 与它内部每个 entry 会在一次配置变更里各打一条 `reload`，噪音翻倍。

## 6. 对 kardia 的对照

| 上游机制 | kardia 现状 | 结论 |
| --- | --- | --- |
| `Fiber.Options.config/commit/dispose` | 已有等价物：`config_hooks`、`ConfigCommitted` 事件、`PluginRemoved` 事件 | 语义已对齐，**不需要**改结构 |
| `ctx[Entry.key]` | kardia 有 `entry_of_context(&ctx)`（`crates/kardia-loader/src/entry.rs`），靠 fiber→entry 映射表 | 等价，但 kardia 是查表不是挂符号 |
| `Entry._hold` / `leak()` | **缺失**。kardia 的 `group_create` 只改 `data.parent`，没有重建 disposer 归属 | **需要跟进**，这是真实缺陷 |
| 自弃用 → `disabled: true` | 已有：`on_removed()`（`crates/kardia-loader/src/loader.rs:455`）实现四种情况判定 | 已对齐 |
| 依赖丢失清 verdict | 已有：`on_status()`（同文件 `:534`） | 已对齐 |
| 组日志去重 | **缺失** | 需要跟进 |

### 6.1 kardia 的持有者缺陷

`crates/kardia-loader/src/group.rs` 的 `group_create()`：

```rust
// the entry may be moved from another group
let data = st.entries.get_mut(&entry).expect("registered");
data.parent = group;
```

只改了 `parent`。entry 的 inner fiber 是通过 `inner_ctx.plugin_def(&def, ...)` 创建的，其 disposer 归属于**当时创建它的那个 ctx 的 fiber**。`group_remove()` 里虽然有「另一个 group 已经接管就不再处置」的保护：

```rust
// an entry that another group already adopted belongs to that group now
if st.entries[&entry].parent != group { return None; }
```

但这只防住了「A 主动移除」，防不住「A 被整体卸载」——那时 A 的 fiber 直接清空自己的 effect 列表，entry 的 fiber 跟着死。上游 `8dbd5d3` 修的正是这条。

**跟进方案**：给 kardia 加 `EffectHandle` 级别的「解绑」能力（对应 `Fiber::leak`），在 transfer 时把 inner fiber 的 disposer 从旧 group 的 effect 列表移到新 group 的 effect 列表。验收需要一个「A 组里启动、转移到 B 组、卸载 A 组、B 组里的插件仍在工作」的测试。详见 `11-implementation-plan.md` P1-1。

## 核对命令

```bash
cd cordis
git show 915e17a -- packages/loader/src
git show 8dbd5d3 -- packages/loader/src/config/entry.ts packages/core/src/fiber.ts
git show origin/main:packages/loader/src/config/entry.ts | sed -n '280,470p'
```
