# 02 core：Context、Fiber 与 std 命名空间

涉及提交：`279ac22`（std）、`915e17a`（Fiber.Options）、`27b9010`（getStack 改名）、`c358e27`（私有化）、`8dbd5d3` 中 core 部分（`leak()`）、`86f59e4`（仅测试）。

改动文件：`packages/core/src/{context,events,fiber,logger,reflect,registry,utils}.ts`，共 15 个文件 +389/−72（含测试）。

## 1. `std` 命名空间

### 1.1 机制

新增 `ReflectService.namespace(name)`：把一个访问器 `name` 变成一个 Proxy，读 `ctx.std.fs` 时实际读 `ctx['std:fs']`。

```ts
class NamespaceHandler implements ProxyHandler<object> {
  get(target, prop) {
    if (prop === symbols.original) return target
    if (isSpecialProperty(prop)) return Reflect.get(target, prop)
    return Reflect.get(this.ctx, this.prefix + prop)
  }
  set(target, prop, value) { ... Reflect.set(this.ctx, this.prefix + prop, value) }
  has(target, prop) { ... Reflect.has(this.ctx, this.prefix + prop) }
}

namespace(name: string) {
  const target = Object.freeze(Object.create(null))
  const prefix = name + ':'
  return this.accessor(name, {
    get() { return new Proxy(target, new NamespaceHandler(this, prefix)) },
  })
}
```

`Context` 构造时调用 `this.reflect.namespace('std')`，接口里加 `std: Namespace<this, 'std'>`。

`Namespace<C, N>` 是纯类型层面的映射：把 `C` 上所有 `N:foo` 形式的键投影成 `foo`，且只取一层——`std:server:request` 这种带第二个冒号的不会出现在 `ctx.std.server.request` 上。

### 1.2 连带改动

`utils.ts` 里追踪代理的关联键从 `associate.prop` 改成 `associate:prop`：

```ts
if (tracker.associate && useSite.reflect.props[`${tracker.associate}:${prop}`]) {
  return Reflect.get(ctx, `${tracker.associate}:${prop}`, ...)
}
```

这条改动让 `ctx.server.publicUrl` 这类「服务上的子访问器」在改名后仍然按同一 def site 治理——对应 `server/request` 变成 `std:server:request`、`server/response` 变成 `std:server:response`。

### 1.3 影响面

所有官方服务的键与文档一起改名：

| 服务 | 旧键 | 新键 |
| --- | --- | --- |
| fs | `fs` | `std:fs` |
| http | `http` | `std:http` |
| server | `server` | `std:server` |
| timer | `timer` | `std:timer` |
| env | `env` | `std:env` |
| hmr | `hmr` | `std:hmr` |

`loader` 与插件名（`server:proxy`、`server:static`、`server:echo`、`server:acl`）**不改**——它们不是「官方服务」，是插件定义名。

`fs` 的拦截键同步改为 `std:fs`，因此 `ctx.intercept('std:fs', { baseUrl })` 才是正确写法（`include`、`server/static` 都改成了这个）。

## 2. `Fiber.Options`

### 2.1 变更

`Fiber` 的构造参数从位置参数 `getOuterStack: () => string[]` 改为 `options: Fiber.Options`：

```ts
export namespace Fiber {
  export interface Options {
    getStack?: (this: void) => string[]
    config?: (this: Fiber, config: any, resolve: (config: any) => any) => any
    commit?: (this: Fiber, config: any) => void
    dispose?: (this: Fiber) => void
  }
}
```

四个钩子的语义：
- `getStack`：错误栈外框来源，替代原 `getOuterStack`。
- `config`：包住 `internal/config` waterfall，可先跑 side effect 再委托 `resolve(config)`。
- `commit`：取代被删除的 `internal/commit` 事件，配置回写走这里。
- `dispose`：fiber 开始卸载时通知，置于 `internal/plugin` 之后、真正卸载之前（`this.uid = null` 之后立刻调用）。

`registry.use(component, config, options)` 与 `registry.plugin(...)` 的第三个参数都变成 `Fiber.Options`，并在内部补 `getStack` 默认值（`buildStack()`）。

### 2.2 为什么这样改

旧实现把 entry 的行为写成 loader 上的五个全局事件监听：

```ts
ctx.on('internal/config', ...)     // 注入与校验追踪
ctx.on('internal/commit', ...)     // 配置回写
ctx.on('internal/update', ...)     // reload 日志
ctx.on('internal/status', ...)     // 依赖丢失时清 verdict
ctx.on('internal/plugin', ...)     // 自弃用检测
```

这些监听必须靠 `fiber.entry` 这个挂在 `Fiber` 上的隐式字段判断「这个 fiber 是不是 entry 的根」，并且要靠 `fiber.parent.fiber?.entry === fiber.entry` 排除子插件。改成 per-fiber 选项后，判断条件变成「这个 fiber 是被谁用带 entry 选项的 `use()` 创建的」，天然精确，且不需要在 `Fiber` 上留字段。

### 2.3 `leak()`

```ts
leak(dispose: Disposable) {
  return this.ctx.fiber._disposables.delete(dispose)
}
```

作用：把一个已经注册到当前 fiber 的 disposer **解绑**，所有权转给调用方。loader 用它把 entry 的 outer fiber 从「创建它的 fiber」转交给「当前持有它的 group fiber」，这是 `8dbd5d3` 修复的基础。

实现细节：方法体里写的是 `this.ctx.fiber._disposables.delete(dispose)` 而不是 `this._disposables`。`Fiber` 有 `ctx`（public readonly）与 `context`（protected，带更具体的类型）两个字段，构造时它们指向同一个 `parent.extend({ fiber: this })`。走 `ctx.fiber` 是为了在类型层面拿到「带 `_disposables` 的 `Fiber` 实例」，而不是依赖 `this` 的可见性——语义上等价，写法上更明确。

## 3. 下划线成员私有化

`c358e27` 把 `EventsService._hooks`、`Fiber._hooks`、`Fiber._disposables`、`LoggerService._snMessage/_snExporter`、`Server._http/_ws`、`Response._end` 等改为 `private`。

TypeScript 的 `private` 是编译期约束，运行时仍可访问，因此上游自己在包内改用下标访问：

```ts
this.fiber['_disposables'].clear()
fiber['_checkImpl'](name)
ctx.registry['_unregister'](runtime)
res['_end']()
```

这带来一个对本项目有用的信号：**这些成员不再是公共 API**，跟进时不必为它们保持兼容。但对下游项目而言，任何通过 `as any` 触达内部字段的代码都会失去类型支持。

## 4. `buildOuterStack` → `buildStack`

纯改名。`composeError(callback, getStack)` 的第二个参数默认值变为 `buildStack()`，`handleError` 的形参同步改名。loader 侧 `Entry.getOuterStack` 变成 `Entry.getStack`，`EntryGroup.getOuterStack(id)` 变成 `getStack(id)`。

## 5. 对 kardia 的对照

| 上游概念 | kardia 现状 | 差距 |
| --- | --- | --- |
| `std:` 服务键 | `Key` 是任意字符串（`crates/kardia-core/src/key.rs`），现有服务键为 `"fs"`、`"server"`、`"http"`、`"timer"`、`"loader"`、`"hmr"`、`"include"`、`"bridge"` | 无命名空间概念；改名是机械替换但要触及 13+ 处 `inject` 与全部配置/文档 |
| `ctx.std.x` 语法糖 | 由 `Key` 直接寻址，`Svc<T>` 已带 caller 语义 | Rust 没有属性代理，无法等价实现；只能靠键命名约定 |
| `Fiber.Options.config` | `config_hooks`（`IdTable<ConfigHook>`）+ `resolve_config` 依次执行 | 语义已存在，但是**全局**注册而非 per-fiber |
| `Fiber.Options.commit` | `ConfigCommitted` 事件 + loader 的 `on_committed` | 已等价 |
| `Fiber.Options.dispose` | `PluginRemoved` 事件 + loader 的 `on_removed` | 已等价，且判定顺序一致（先 unregister 再 dispose） |
| `Fiber.Options.getStack` | 无此概念（Rust 无 JS 栈帧拼接） | 不跟进 |
| `leak()` | `EffectScope::adopt` 存在，但没有「解绑」 | 缺失，见 `03-loader-entry.md` |
| 下划线私有化 | Rust 用 `pub(crate)` / 模块可见性 | 无需跟进 |

### 5.1 关于 `std` 前缀的决策依据

跟进 `std:` 的理由：
1. 上游是唯一权威。配置示例、Schema 描述、`inject` 数组写法全部指向 `std:fs`，不跟会让两边配置文件不可互换。
2. 隔离/拦截按 `Key` 字符串匹配，键名不一致会导致跨实现的 intercept 静默失效。

不跟的理由（需要权衡）：
1. kardia 没有 `ctx.std.x` 语法糖，`"std:fs"` 在 Rust 侧只是更长、更易错的字面量。
2. 本项目对外承诺的是「Cordis 的 Rust 实现」，配置层面兼容性比内部命名更重要；而配置里 `inject` 可以由 loader 归一化。

建议：**在 loader 的 `inject` 解析与拦截键匹配处做单向归一化**——外部配置可以写 `fs` 或 `std:fs`，内部统一存 `std:fs`；不要求 kardia 内部所有 `Plugin::inject()` 都改成裸字符串。这样既兼容上游配置，又不把 Rust 代码写得满屏冒号。详见 `11-implementation-plan.md` 的 P0-2。

## 核对命令

```bash
cd cordis
git show 279ac22 -- packages/core/src/reflect.ts packages/core/src/context.ts
git show 915e17a -- packages/core/src/fiber.ts
git show c358e27 --stat
git show 8dbd5d3 -- packages/core/src/fiber.ts
```
