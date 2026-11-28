# 08 工具链与发布流程

涉及提交：`54cbf0f`（版本号）、`5fcee75`（yakumo 升级）、`eaf642c` 与 `6ad12cf`（发布流程）、`38abad0` 中的 CI 矩阵部分、以及 `b1b8a44`（文档）。

## 1. 版本

`packages/core/package.json` 的 `version`：`4.0.0-rc.13` → `4.0.0-rc.14`。

其余包同步 bump（`cli`、`env`、`fs/core`、`fs/memory`、`fs/node`、`hmr`、`http`、`http-socks`、`include`、`loader`、`logger-console`、`package-manager`、`timer`、`cli-cordis`）。

本项目影响：`@kardiajs/*` 是 0.2.0，与 cordis 版本无联动关系。**不需要跟进 bump**，但「跟进上游后 kardia 侧对应能力变了」应在 kardia 自己的版本里体现。

## 2. yakumo 升级

`5fcee75` 做了两件事：

1. `.yarn/patches/` 下 4 个补丁（`yakumo-esbuild`、`yakumo`、`yakumo-tsc`、`yakumo-vitest`）全部删除。
2. `yakumo` 升级到上游已包含这些修复的版本，`yarn.lock` 随之重算。

`yakumo.yml` 增加两行（`tsconfig.json` 同步），用于新的构建目标。

本项目影响：无。kardia 的 JS 侧用 `bun` + 自建 `vitest.config.*`，不依赖 yakumo。

## 3. 发布流程重写

### 3.1 旧流程（基线）

```
on: push[main]                          → detect job：从 commit 反查带 release 标签的 PR
on: pull_request[opened, edited, ...]   → check job：PR 标题必须是 "chore: release vX.Y.Z"，
                                                   头分支必须是 "release/vX.Y.Z"，
                                                   body 必须有内容，tag 不能已存在，
                                                   cordis 版本必须变过
```

`publish` 依赖 `detect.outputs.number`，即「push 到 main 后才知道要发布」。

### 3.2 新流程

```
on: pull_request[opened, edited, synchronize, reopened, labeled, unlabeled, closed]

check   : if action != 'closed'    → 校验版本一致性
publish : if action == 'closed' && merged && base == 'main' && has label 'release'
```

发布条件收窄为「带 `release` 标签的 PR 被合并进 main」。

### 3.3 `check-release.mjs` 的判据变化

旧判据：

```js
if (process.env.PR_TITLE !== `chore: release ${tag}`) errors.push(...)
if (process.env.PR_HEAD_REF !== `release/${tag}`) errors.push(...)
if (!process.env.PR_BODY?.trim()) errors.push(...)
```

新判据：

```js
// a published package is public at the base; a package is published by this pull request
// if it is public at the head and its version at the head is not published yet
const published = manifestPaths.filter((path) => {
  if (!isPublic(head[path])) return false
  return !isPublic(base[path]) || base[path].version !== head[path].version
})

const cordisPath = 'packages/core/package.json'
const isCordisRelease = published.includes(cordisPath)
const tag = `v${head[cordisPath].version}`
```

关键变化：

| 变化 | 含义 |
| --- | --- |
| 不再要求标题/分支名格式 | 去掉对 PR 命名的人为约束 |
| 用 `git ls-tree` 判断清单是否存在于某 commit | 新建包（base 不存在）也算发布 |
| 用 `git ls-files` 过滤工作区清单 | 只统计本仓库跟踪的文件，避免 `yarn workspaces list` 带出外部目录 |
| 只有 `cordis` 版本变化才打 tag 与写 release notes | 单独发布 `@cordisjs/loader` 之类的包不再创建 GitHub Release |

`publish` job 的 checkout 改为 `fetch-depth: 2`，并注释说明「合并提交与它的第一父提交，即基分支」。

### 3.4 tag 与 release notes

```bash
previous=$(git show HEAD^1:packages/core/package.json | node -p "JSON.parse(require('fs').readFileSync(0,'utf8')).version")
if [ "$version" = "$previous" ]; then
  echo "cordis remains $version"
  exit 0
fi
```

即「`cordis` 没变就不发 Release」。

## 4. CI 矩阵

`38abad0` 带了一处 CI 改动：在 `build.yml` 增加 Node.js 22 的测试组合，并允许 macOS 失败。

```yaml
strategy:
  matrix:
    node: [22, 24]
```

行数变化为 `+3/−1`，属于最小化调整。

## 5. 本项目对照

| 上游项 | kardia 现状 | 结论 |
| --- | --- | --- |
| 版本 bump | `0.3.0` / `@kardiajs/* 0.2.0` | 独立版本线，不跟 |
| yakumo | 未使用（`bun` + `vitest`） | 不跟 |
| 发布触发方式 | `.github/workflows/` 下自有流程 | 可借鉴「合并即发布」的思路 |
| `fetch-depth: 2` | 若采用同样流程需要 | 参考 |
| Node 22 矩阵 | `js/` 侧测 Node 24 | 若下游需要 Node 22 兼容，加一行 |

### 5.1 值得借鉴的一点

上游新流程解决了一个真实问题：**「push 到 main 之后反查 PR」不可靠**——squash merge、rebase merge、直接 push 都会让 `commits/{sha}/pulls` 返回空或返回错误的 PR。改成「在 PR 的 `closed` 事件上判断 `merged`」是事件驱动的正确做法。

如果 kardia 的发布流程也有「从 commit 反查 PR」的步骤，建议按同样方式改造。若不是，本轮不动 CI。

## 6. 文档变更

`b1b8a44` 把 schemastery 里原本是中文的 `.description()` 全部改成英文，并补全缺失描述。涉及 `http`、`server/static`、`server/proxy` 等。

影响：**上游 schema 描述语言统一为英文**。kardia 的 Rust 侧配置结构体没有描述字段，但 README 与示例配置里有中文注释。若要长期对齐上游文档，应明确「面向用户的配置说明以英文为准，中文版单独维护」。

## 核对命令

```bash
cd cordis
git show 54cbf0f --stat
git show 5fcee75 --stat
git diff 36dc7fb..origin/main -- .github/workflows/release.yml
git show 6ad12cf -- .github/scripts/check-release.mjs
```
