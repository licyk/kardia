# 12 执行记录

本文记录 `11-implementation-plan.md` 的实际落地结果，包括与计划的偏差及其原因。计划本身保留不改，便于对照。

## 1. 已完成

| 计划项 | 提交 | 实际结果 |
| --- | --- | --- |
| 基线更新（pin、inventory、适配层） | `6b3c9af` | 完成，含 `it.each` 的 `as const`/`%j`/行式用例适配 |
| P0-1 查询串含 `?` | `2959524` | 完成 |
| P0-2 表单 content-type 与 boundary | `2959524` | 完成，用 `form_type` 精确识别「这个头是我写的」 |
| P0-3 IPv6 主机括号 | `2959524` | 完成，覆盖 80/443/其他三个分支 |
| P0-4 timer 重入保留尾部句柄 | `2959524` | 完成 |
| P1-1 `leak()` | `58cd9c0` | 只加了原语，**未加 loader 持有者机制**，理由见第 3 节 |
| P1-3 畸形百分号 400 | `82ee9f8` | 完成，同时让路由捕获失败转为「不匹配」 |
| P2-2 static 目录先于扩展名 | `3c2d860` | 完成 |
| P2-3 http `noProxy` | `2896418` | 完成 |
| P3-1 事件带 `FiberId` | `eaedf32` | 完成 |
| P3-2/P3-3 bridge 增强 | `8ff4d17` | 完成 |

## 2. 验证状态

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo xtask arch` | 通过 |
| `cargo test --workspace` | 通过，无失败目标 |
| `cargo clippy -p kardia-loader --no-default-features` | 通过 |
| `cargo test -p kardia-loader --no-default-features` | 通过 |
| `cargo clippy -p kardia-hmr --no-default-features --features process` | 通过 |
| `bun run check` / `bun run test` | 通过，155 个测试 |
| `bun run server:inventory --check` | 通过，156 个用例 |
| `bun run test:server:reference` | 183 / 224 通过 |

## 3. 与计划的偏差

### 3.1 P1-1 没有引入 loader 持有者机制

计划要求照搬上游 `Entry._hold` / `_release`，并新增 `EffectHandle::leak()` 支撑它。

实际验证：kardia 的 entry context 在转移时通过 `patch_fibers()` 调用 `data.ctx.reparent(&parent)` 重建 ctx 链，因此 group 卸载不会连带 entry。写了一个「A 组启动 → 转移到 B 组 → 卸载 A 组 → B 组插件仍在运行」的测试，**在没有任何 loader 改动的情况下通过**。

结论：上游 `8dbd5d3` 修的是 Cordis 的 context 原型链模型特有的缺陷，kardia 的 reparent 模型不存在它。因此只保留 `EffectHandle::leak()`（与上游 API 对齐、可独立使用），不引入无消费者的持有者簿记——符合仓库规则「不为臆测的问题增加复杂度」。

### 3.2 P2-1 fs 能力集未实施

计划列了 `stat`/`write`/`rename`/`remove`/`realpath` 与 `capabilities`。

实际未做。原因是它牵动 `FsProvider` 的 trait 形状与 `FsExt` 的公开 API，而当前 parity 失败的 fs 项（6 个）出自**`Path` 语义**（字符串里的 `?`/`#`/`%` 现在是字面字符），不是缺 entry 操作。先改能力集不能减少任何失败。

### 3.3 P2-4 proxy 前缀化未实施

计划要求把 `target` 从「目录」改成「前缀」，并加 `requestHeaders`/`isolateOrigin`/hop-by-hop 展开。

实际只做了**适配层适配**（`upstream-host.mjs` 按新语义构造 target），Rust 侧仍是旧语义。这解释了 9 个 proxy 对照失败：上游按新语义转发，kardia 按旧语义。

### 3.4 未做 `ServerProxy` 删除

用户决定「跟随上游删除」，但该决定与 P2-4 绑定：`ServerProxy` 是 `proxy/plugin.rs` 里的服务，其 `public_url()` 已被 parity 适配层与新测试改为使用 `server.public_url()`。**服务本身仍在**，因为删除它需要同时改 `proxy.rs` 测试与任何下游引用，应与 P2-4 一起做。

## 4. 剩余 parity 失败的性质

| 套件 | 失败数 | 根因 |
| --- | --- | --- |
| `client-parity` | 26 | 依赖 proxy/fs 的对照结果；另有上游 http 客户端行为差异待逐条核对 |
| `proxy-parity` | 9 | Rust 侧仍是「目录」语义，见 3.3 |
| `fs-parity` | 6 | `Path` 三态语义未实施，见 3.2 |

这三组都不是回归：基线在 `36dc7fb` 时是 224 / 224 全绿，pin 更新后暴露的是**上游已变更、kardia 未跟进**的部分。

## 5. 下一步

按 `11` 的顺序，剩下的是：

1. **P2-4**：proxy 前缀化 + 头映射 + `isolateOrigin` + 删 `ServerProxy`。做完后 `proxy-parity` 与大部分 `client-parity` 应恢复。
2. **P2-1**：fs `Path` 三态与 entry 操作。做完后 `fs-parity` 应恢复。
3. **P1-2**：`std:` 归一化。未做，因为它不改变任何测试结果，纯命名一致性。

## 6. 复核命令

```bash
cd /root/code_workspace/kardia
git log --oneline 6b3c9af..HEAD
cargo test --workspace
cd js && bun run test:server:reference
```
