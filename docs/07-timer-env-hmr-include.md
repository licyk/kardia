# 07 timer、env、hmr、include

涉及提交：`f35ee3a`、`bdd4126`（timer）、`3ef1c4b`（env）、`38abad0`、`5e2e6c1`（hmr）、`c9fe179`（include）、以及四者的 `std` 迁移。

## 1. timer

### 1.1 节流窗口改用单调时间（`f35ee3a`）

基线用 `Date.now()` 记录上次调用的时刻。系统时钟被 NTP 回拨时窗口会算错，甚至永久阻塞。

修复方向：改用 `performance.now()`（单调递增）。这个改动在 JS 侧是行为修正，在 Rust 侧本来就不是问题——kardia 已经用 `tokio::time::Instant`：

```rust
// crates/kardia-timer/src/service.rs
let last_call: Arc<Mutex<Option<Instant>>> = Default::default();
...
let elapsed = last_call.lock().map(|at| at.elapsed());
```

`tokio::time::Instant` 在启用 `test-util` 时可被 `pause()`/`advance()` 操纵，且在非 test 模式下是单调时钟。**这一项 kardia 已具备。**

### 1.2 重入时保留尾部句柄（`bdd4126`）

```ts
const wrapper: any = (...args: any[]) => {
  if (isDisposed) return
  clearTimeout(timer)
  timer = undefined
  const next = trigger(args)
  // 重入的回调可能在 trigger 返回前就排好了尾部调用；保留它的句柄
  if (next !== undefined) timer = next
}
```

问题场景：`throttle(fn, 100, true)` 的回调里再次调用 wrapper。基线 `timer = trigger(args)` 会在 `trigger` 返回后覆盖掉重入调用刚写进去的句柄，导致尾部定时器失联——既不会被后续调用取消，也不受 disposer 管理。

修复分两步：先 `timer = undefined`，再在 `trigger` 返回后只在「确实拿到了新句柄」时写回。

kardia 的对应实现（`crates/kardia-timer/src/schedule.rs`）：

```rust
pub fn call(&self, args: A) {
    let mut state = self.state.lock();
    if state.disposed { return }
    if let Some(timer) = state.timer.take() { timer.abort(); }
    drop(state);                       // 出锁，允许重入
    let timer = (self.trigger)(args);
    let mut state = self.state.lock();
    if state.disposed {
        if let Some(timer) = timer { timer.abort(); }
    } else {
        state.timer = timer;           // 无条件写回
    }
}
```

**差异**：kardia 在重入结束后**无条件**用外层调用的结果覆盖 `state.timer`。如果内层重入先写入了尾部句柄，外层返回的 `None` 会把它清掉——与上游基线是同一个 bug，只是形态不同（Rust 里 `state.timer = None`）。

修复方式：只在 `timer.is_some()` 时写回，或引入「代次」比较：

```rust
let timer = (self.trigger)(args);
let mut state = self.state.lock();
if let Some(timer) = timer {
    if state.disposed { timer.abort(); } else { state.timer = Some(timer); }
}
```

但要注意：若外层是「立即执行」分支（返回 `None`），则应清掉之前排的尾部——此时 `take()` 已经 abort 了。所以正确语义是「`None` 不写回，但也不额外保留」，直接把 `state.timer` 留在 `take()` 之后的状态即可。需要一个新的重入测试来固定行为。

### 1.3 `std:timer`

```ts
super(ctx, 'std:timer')
ctx.mixin('std:timer', ['timeout', 'interval', 'throttle', 'debounce', 'setTimeout', 'setInterval'])
```

注意 `mixin` 的名字也改了——它是 `createTraceable` 的 `associate` 键，必须与服务键一致才能让 `ctx.timeout(...)` 的 def site 归到 `std:timer` 上。

## 2. env

### 2.1 空变量覆盖文件（`3ef1c4b`）

```ts
// A variable of the process environment, including one of an empty value,
// takes precedence over the files. `dotenv-expand` respects only a truthy
// one, so the process environment is assigned once more after the expansion.
const env: Record<string, string | undefined> = { ...process.env }
expand({ parsed: merged, processEnv: env as Record<string, string> })
Object.assign(env, process.env)
```

`dotenv-expand` 内部判断「变量是否已设置」用的是真值检查，空串因此被当成未设置，文件里的值反而覆盖了 `process.env` 的空值。补一次 `Object.assign` 修正。

这是**一行修复**，但语义重要：`FOO=` 在 shell 里设成空串，必须能让配置文件里的 `FOO=bar` 失效。

### 2.2 `std:env`

```ts
declare module 'cordis' { interface Context { 'std:env': Readonly<Record<string, string | undefined>> } }
export const inject = ['std:fs']
ctx.provide('std:env', Object.freeze(env))
```

**kardia 没有 env 插件**（`grep '"env"' crates/*/src` 结果为 0）。若要做，这一项是从零新增，不是跟进修复。

## 3. hmr

### 3.1 符号链接的真实路径（`5e2e6c1`）

```ts
function canonical(path: string) {
  try { return realpathSync(path) } catch { return path }
}

ctx.std.fs.watch(...).on('change', (url) => {
  const filename = fileURLToPath(url)
  // 模块 URL 是真实路径，而 watcher 报告的是它被给定的那个拼写
  stashed.add(canonical(filename))
  ...
})
```

问题：`ModuleCaches` 索引的是真实路径（Node 的 ESM loader 会 realpath），而 watcher 报告的是监听起点拼出来的路径。项目用符号链接组织目录时（pnpm workspace、monorepo），改动永远命中不到模块。

修复：入 `stashed` 前 realpath 归一。注意失败时回退原路径——`realpathSync` 在文件刚被删除时会抛。

### 3.2 Node 22 同步模块钩子（`38abad0`）

```ts
const resolveOptions = (options: any) => options?.shouldSkipModuleHooks ? options : false
...
resolveForCJSWithHooks(request, parent, isMain, resolveOptions(args[0]))
```

Node 各版本对 `Module._load` 的第四参 `resolveForCJSWithHooks` 第 4 参数解释不同：

| Node 版本 | 第 4 参形态 |
| --- | --- |
| 22.22、23、24.12 | 不存在 |
| 22.23 | 被当作真值判断的 flag |
| 24.15+、26 | 被解构的 options 对象 |

统一转换：`{ shouldSkipModuleHooks: true }` → 原样；其他 → `false`（即「不跳过钩子」）。

### 3.3 `std:hmr`

```ts
super(ctx, 'std:hmr')
static inject = ['std:fs']
this.baseDir = fileURLToPath(ctx.std.fs.baseUrl!)
```

`baseUrl` 的非空断言注释说明了前提：「Node.js 有工作目录，所以 baseUrl 一定存在」。

### 3.4 kardia 对照

kardia 的 hmr 是**完全不同的实现**：进程内用 `libloading`/`wasmtime` 直接加载 `.so`/`.dylib`/`.wasm`，没有「Node ESM loader 补丁」这一层。

| 上游修复 | kardia 相关性 |
| --- | --- |
| 符号链接真实路径 | **相关**。`crates/kardia-hmr/src/hmr.rs:462` 有 `normalize(path: &Path) -> PathBuf`，需确认它是否 realpath |
| Node 22 钩子 | 不相关（无 Node 依赖） |
| `std:hmr` | 机械改名，`inject` 从 `"fs"` → `"std:fs"` |

需要实测：`normalize()` 是否对符号链接做 `canonicalize`。若只做 `Path` 标准化（去掉 `..`），则存在同类缺陷。

## 4. include

### 4.1 无前置锚点的别名（`c9fe179`）

```ts
function unresolvedAliases(document: Document) {
  const anchors = new Set<string>()
  const unresolved: Alias[] = []
  visit(document, (_, node) => {
    if (isAlias(node)) {
      if (!anchors.has(node.source)) unresolved.push(node)
    } else if (isNode(node) && node.anchor) {
      anchors.add(node.anchor)
    }
  })
  // 扫描结束后所有锚点都已知道
  return unresolved.map((node) => {
    const [start, end] = node.range!
    const message = anchors.has(node.source)
      ? `Alias *${node.source} must follow its anchor`
      : `Unresolved alias: *${node.source}`
    return new YAMLParseError([start, end], 'BAD_ALIAS', message)
  })
}
```

`yaml` 解析器接受「引用出现在锚点之前」的文档，只在 `toJS()` 时抛一个**没有位置信息**的 `ReferenceError`。这里在解析后主动扫描，产出带 range 的 `YAMLParseError`，区分两种情况：

- 锚点存在但在后面 → `must follow its anchor`
- 锚点根本不存在 → `Unresolved alias`

### 4.2 `ctx.entry` → `ctx[Entry.key]`

include 里 8 处 `ctx.fiber.entry` 全部替换（`enableLogs`、`commit`、`_drain`、`trace` 等）。另外 `ctx.intercept('fs', ...)` → `ctx.intercept('std:fs', ...)`。

### 4.3 kardia 对照

| 上游修复 | kardia 现状 | 结论 |
| --- | --- | --- |
| 悬空别名带位置报错 | `crates/kardia-include/src/yaml.rs:187-192`，`Event::Alias` 找不到锚点时直接返回 `ParseError { message: "unknown alias", position: start }` | **已具备位置信息**；但不区分「锚点在后」与「不存在」 |
| `std:fs` 拦截 | `include.rs:200` `ctx.intercept("fs", ...)` | 随改名 |
| `ctx.entry` | 无对应概念 | 不适用 |

差异很小。若要完全对齐，把 `saphyr-parser` 的 `Alias` 分支拆成两种消息即可。**低优先级。**

## 5. 汇总优先级

| 项 | 归属 | 优先级 |
| --- | --- | --- |
| 重入时保留尾部句柄 | timer | **P1**（现存缺陷） |
| 符号链接真实路径 | hmr | **P1**（需先实测） |
| 别名错误消息区分 | include | P3 |
| 节流单调时钟 | timer | 无需跟进（已具备） |
| 空 env 覆盖 | env | 无 env 插件，暂不适用 |
| Node 22 钩子 | hmr | 不适用 |

## 核对命令

```bash
cd cordis
git show bdd4126 -- packages/timer/src/index.ts
git show 5e2e6c1 -- packages/hmr/src/index.ts
git show c9fe179 -- packages/include/src/index.ts
cd ..
grep -n "state.timer = timer" crates/kardia-timer/src/schedule.rs
grep -n "fn normalize" -A 10 crates/kardia-hmr/src/hmr.rs
```
