# 04 fs：从单一服务到完整文件系统抽象

涉及提交：`d6de1c0`（包拆分）、`33dd8ee`（字符串按路径解析）、`2ce387c`（entry 操作）、`0bc6f0a`（memory provider）、`0aa3e5c`（区间读）、`f4c6b3a`（原子/独占写）、`a68681d`（条件读写）、`285a949`（readlink/realpath 返回对象）、`6feacd0`（DirRef）、`32dd638`/`bc94ca2`（watch 修复）、`279ac22`（`std:fs`）。

改动规模：31 个文件 +7055/−884，是本轮最大的一块。

## 1. 包拆分

```
packages/fs/                 packages/fs/
├── src/index.ts             ├── core/           @cordisjs/fs
├── src/file.ts       →      ├── core/src/*.ts
├── src/watcher.ts           ├── memory/         @cordisjs/fs-memory   新增
├── src/error.ts             ├── node/           @cordisjs/fs-node
└── tests/                   └── conformance/    @cordisjs/fs-conformance 新增
```

原来的 `packages/fs-node` 移到 `packages/fs/node`。新包 `fs-memory` 提供内存 provider，`fs-conformance` 把 provider 必须满足的行为抽成可复用测试套件（992 行）。

`fs/core/src` 拆成 9 个文件：

| 文件 | 职责 |
| --- | --- |
| `index.ts` | `Fs` 服务、mount 管理、`_read`/`_call` 路由（999 行） |
| `path.ts` | `Path` 类型与解析：字符串路径、名字数组、URL |
| `entry.ts` | `Stats`、`EntryType` 与各操作选项类型 |
| `ref.ts` | `EntryRef`、`FileRef`、`DirRef`、`DirEntryRef` |
| `slice.ts` | 区间读的解析与流切片 |
| `write.ts` | 临时文件、原子替换、独占创建 |
| `version.ts` | `ifMatch` 的版本比较 |
| `filter.ts` | watch 的 glob 过滤 |
| `watcher.ts` | `Watcher` |
| `error.ts` | `FsError` 与错误码 |

**跟进要点**：本项目 `crates/kardia-fs` 只有 `access.rs`、`error.rs`、`file.rs`、`filter.rs`、`lib.rs`、`local.rs`、`service.rs`、`url.rs`、`watcher.rs`——结构上更接近上游**基线**（一个 `file.rs` 装 `FileRef` + `FsProvider`），不是跟进后的形状。

## 2. `Path` 语义（破坏性）

### 2.1 变更

基线用 `toReference(path)` 把字符串当「相对引用」，Windows 盘符需要特判。跟进后统一为 `Path = string | readonly string[] | URL`：

- 带 ≥2 字符 scheme 的字符串（如 `memory:`）→ 绝对 URL。
- 其他字符串 → **路径**，`/` 与 `\` 都当分隔符，`.`/`..` 有目录语义，前导分隔符从 base 根开始。
- 名字数组 → 每个元素是字面名字，`\` 也是字面字符；空、`.`、`..`、含 `/` 的元素报 `InvalidPath`。

### 2.2 为什么

基线把字符串一律当相对 URL 引用解析，导致「文件名里带 `%`、`#`、`?`、空格」需要调用方预编码，且 `a:b` 会被误判成 scheme。新语义明确区分「路径」与「名字数组」，把编码责任收进 `encodeName()`：

```ts
function encodeName(name: string) {
  return name.replace(/[\x00-\x20%?#\\]/g, (char) =>
    '%' + char.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0'))
}
```

## 3. Entry 操作

新增（`packages/fs/core/src/index.ts`）：

| 方法 | 说明 |
| --- | --- |
| `access(path, mode)` | 只有 `'write'` 模式；用于「文件是否可写」探测 |
| `rename(src, dest)` | 改名/移动 |
| `remove(path, { recursive, force })` | 删除 |
| `stat(path, { follow })` | `Stats`，`follow: false` 等价 `lstat` |
| `exists(path)` | `stat` 的布尔包装 |
| `mkdir(path, { recursive })` | 建目录 |
| `symlink(target, path)` | 建符号链接 |
| `readlink(path)` | 返回 `{ target }` |
| `realpath(path)` | 返回 `{ url }` |
| `copy(src, dest, { recursive, dereference, exclusive, atomic })` | 复制 |
| `readdir(path)` | `AsyncIterable<DirEntryRef>` |

`FsProvider` 的能力通过 `capabilities` 声明，未声明的操作由 `Fs` 报 `NotSupported`。

`Stats` 增加 `version?: string`（不透明版本串，`ifMatch` 与 `WriteResult.version` 使用）与 `ino`/`dev`（bigint）。

## 4. 引用对象

```
EntryRef (url, name)
├── FileRef  : slice(), stream(), bytes(), arrayBuffer(), text(), json(), write()
└── DirRef   : dir(name), file(name), list()
```

`FileRef` 的三个设计点：

1. **`slice()` 惰性**：只记录 `_slices`，每次读时按 `Blob.slice()` 语义解析（负数从末尾算，越界截断）。写入 slice 抛 `TypeError`。
2. **`stream()` 所有权转移**：`0aa3e5c` 把读到的 chunk 所有权交给消费者，provider 可以复用缓冲区——这是破坏性变更，但只在 provider 实现层面。
3. **`bytes()` 零拷贝**：单块且占满整个 buffer 时直接返回 `new Uint8Array(buffer)`，否则合并。

## 5. 写入语义

`WriteOptions`：

```ts
interface WriteOptions {
  exclusive?: boolean   // 已存在则 AlreadyExists
  mode?: number         // POSIX 低 12 位
  atomic?: boolean      // 读者看到新或旧内容，失败保留旧内容
  ifMatch?: string      // 版本不符则 VersionMismatch
}
```

原子写的实现（`write.ts`）：

1. 在目标**旁边**创建临时文件 `.cordis-<12位十六进制>.tmp`。
2. 内容写入临时文件。
3. `rename` 替换目标，遇到 `Busy` 线性退避重试（`RENAME_RETRIES = 10`，`RENAME_BACKOFF = 20ms`）。

临时文件名是固定模式 `^\.cordis-[0-9a-f]{12}\.tmp$`，watcher 与 include 都靠 `isTemporaryFile()` 忽略自己造成的抖动。

`createTemporary()` 检查目标是否在同一 mount 内（`owns()`），跨 mount 无法原子替换 → `NotSupported`。

## 6. 条件读写

```ts
interface ReadOptions {
  start?: number
  end?: number
  ifMatch?: string   // 需要 provider 能力 conditional.read
}

interface WriteOptions {
  ifMatch?: string   // 需要 provider 能力 conditional.write
}
```

provider 在 `stats.version` 报告版本，`Fs` 侧在**任何字节交付之前**比较，不符就 `VersionMismatch`。这条能力是 include 的「乐观并发写」基础（上游 include 用「读到的内容 === 期望内容」做检查，跟进后可以用版本替代）。

## 7. `DirRef`

```ts
fs.dir('/a/b').file('c.txt')   // 等价 fs.file('/a/b/c.txt')
fs.dir('/a/b').dir('c')        // 子目录
for await (const entry of fs.dir('/a/b').list()) { ... }
```

`list()` 产出 `DirEntryRef`，带 `name`、`type`、`url()`。

## 8. watch 修复

| 提交 | 内容 |
| --- | --- |
| `32dd638` | watch 结束后丢弃排队的 `ready` 回调，避免对一个已停止的 watch 报告就绪 |
| `bc94ca2` | fs-node 不监听 FIFO/socket/设备；用「是否收到事件」而不是「是否挂起」来测试 |

## 9. `std:fs`

`super(ctx, 'fs')` → `super(ctx, 'std:fs')`，`BaseFs.inject = ['std:fs']`，`ctx.fs.mount()` → `ctx.std.fs.mount()`，拦截键 `ctx.intercept('fs', ...)` → `ctx.intercept('std:fs', ...)`。

`baseUrl` 变成 `URL | undefined`：在浏览器等没有工作目录的运行时为 `undefined`。因此 `fs.baseUrl!` 在 Node 侧出现非空断言，`cli-cordis` 用 `??=`。

## 10. 对 kardia 的对照

| 上游能力 | kardia 现状 | 差距等级 |
| --- | --- | --- |
| 包拆分 | `kardia-fs` 单 crate，`local.rs` 内建本地 provider | 结构差异，非语义 |
| `Path` 三态语义 | `FsExt::resolve(&str)`，只有字符串；`url.rs` 有 `resolve`/`directory` | **大**：无名字数组、无「字面名字」概念 |
| `access/rename/remove/stat/mkdir/symlink/readlink/copy/readdir` | **全部缺失**；`FsProvider` 只有 `read()` | **最大** |
| `FileRef::stream/bytes/text/json/write` | `FileRef::stream()` 一个方法 | **大** |
| `slice()` 区间读 | 无 | 中 |
| 原子写/独占写 | 无 | 大（include 现在自己实现临时文件 + rename） |
| `ifMatch` 条件读写 | 无 | 中 |
| `DirRef` | 无 | 中 |
| `capabilities` 声明 | 无 | 中（新能力需要它才不至于遍布 `NotSupported`） |
| `std:fs` 键 | `"fs"` | 小（机械） |
| watch 结束丢回调 | 已具备（实现方式不同）：`watcher.rs:123-128` 的 `Signal::Ready` 先从 `hooks.ready` 取 listener，已移除则取不到，回调不执行 | — |
| FIFO/socket/设备跳过 | **缺失**：`watcher.rs` 无文件类型判断，`notify` 会尝试监听任何路径 | S |

### 10.1 影响判断

kardia 的 fs 目前只服务三个消费者：`include`（读写配置文件）、`server-static`（读文件服务）、`hmr`（监听文件）。三者都只需要「读」，唯一的写是 include 自己拼临时文件。

所以**不必一次性对齐上游完整 entry 操作集**。建议按消费者倒推：

1. `include` 需要 `read`/`write`/`rename`/`remove`/`access`/`stat`（判断只读）。
2. `server-static` 需要 `stat`（区分文件/目录，替代现在靠 `read` 报 `IsADirectory` 的做法）与区间读（HTTP Range 支持的前置）。
3. `hmr` 需要 `realpath`（符号链接真实路径，对应 `5e2e6c1`）。

完整迁移计划见 `11-implementation-plan.md` 的 P2 阶段。**不建议**在本轮把 `kardia-fs` 拆成三个 crate——Rust 侧没有独立的 provider 生态，拆包只会增加链接复杂度。

## 核对命令

```bash
cd cordis
git show 33dd8ee -- packages/fs/core/src/path.ts
git show 2ce387c --stat
git show f4c6b3a -- packages/fs/core/src/write.ts
git show a68681d -- packages/fs/core/src/version.ts
cd ..
grep -n "pub fn " crates/kardia-fs/src/access.rs crates/kardia-fs/src/file.rs
```
