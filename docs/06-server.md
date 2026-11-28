# 06 server：core、static、proxy、acl

涉及提交：`279ac22`（`std:server`）、`65edc8f`（畸形百分号 400）、`a15b178`（查询串含 `?`）、`418a010`（IPv6 主机）、`127e57e`（表单体只编码一次）、`93bc78a`（static 目录重定向顺序）、`7e4387d`（proxy 前缀化）、`c358e27`（`_end` 私有化）。

## 1. server/core

### 1.1 畸形百分号路径 → 400

```ts
// The server accepts paths whose percent escapes are well-formed and encode valid UTF-8.
function isEncoded(path: string) {
  try { decodeURI(path); return true } catch { return false }
}
```

HTTP 请求处理前：

```ts
if (isEncoded(req.path)) {
  await this.ctx.waterfall(this, 'server/request', req, res, async () => {})
} else {
  res.status = 400
}
```

WebSocket upgrade 前：

```ts
if (!isEncoded(req.path)) {
  socket.write('HTTP/1.1 400 Bad Request\r\n\r\n')
  socket.destroy()
  return
}
```

配套还有一处：路由参数解码不能抛。

```ts
for (const [index, { name }] of this.keys.entries()) {
  const value = capture[index + 1]
  if (value === undefined) continue
  try {
    params[name] = decodeURIComponent(value)
  } catch {
    // 路由字面量可能把百分号转义或 UTF-8 序列从中间切开
    return
  }
}
```

`return` 不是 `throw`——意味着「这条路由不匹配」，继续试下一条。这是关键语义：`/a%2Fb` 在一段路由里可能捕获出半个 `%2F`，此时应该让后面的路由有机会匹配，而不是整个请求 400。

### 1.2 查询串含 `?`

```ts
this.path = this.url.split('?')[0]
this.query = new URLSearchParams(this.url.slice(this.path.length))
```

基线用 `url.split('?')[1]`，`?a=1?b=2` 会被截成 `a=1`。`slice(this.path.length)` 从第一个 `?` 起到结尾，完整保留。

### 1.3 IPv6 主机

```ts
function formatHost(host: string) {
  return host.includes(':') ? `[${host}]` : host
}

const address = `http://${formatHost(this.host)}:${this.port}`
```

`publicUrl` 的三个分支（80/443/其他）都改用 `authority`。

### 1.4 表单响应体只编码一次

新增 `_formType` 记录上次为表单体生成的 content type：

```ts
} else if (method === 'formData') {
  data = encodeFormData(data)
  // 新 body 有自己的 boundary，替换掉上一次表单体的 content type
  if (this.headers.get('content-type') === this._formType) {
    this.headers.delete('content-type')
  }
  this._formType = data.type
}
```

`encodeFormData()` 自己拼 multipart，而不是交给平台编码器——因为平台编码器每次序列化都生成新 boundary，无法提前设置 `content-type`。

### 1.5 `std:server`

服务键 `server` → `std:server`；`Request`/`Response` 的 tracker 关联键 → `std:server:request` / `std:server:response`；`_http`/`_ws`/`_end` 私有化。

## 2. server/static

### 2.1 目录重定向先于扩展名尝试

```ts
async function tryFile(url: URL) {
  let result: Response | FsError | undefined
  if (!isExcluded(relativePath(url))) {
    result = await read(url, config.disposition)
    if (result instanceof Response) return result
    if (result?.code === 'IsADirectory') return result       // 新增
  }
  for (const ext of config.extensions) { ... }
  return result
}
```

基线在目录上会继续尝试 `index.html` 之类的扩展名，导致 `/docs` 命中 `/docs.html` 而不是重定向到 `/docs/`。现在 `IsADirectory` 立即返回，由上层决定重定向。

### 2.2 `std:fs` 拦截

```ts
const root = ctx.intercept('std:fs', { baseUrl: config.root }).std.fs.baseUrl!
```

非空断言是新增的，因为 `baseUrl` 现在是 `URL | undefined`（浏览器无工作目录）。

## 3. server/proxy

这是本轮**最彻底的破坏性重构**。跟进后 `server-proxy` 不再是「转发到某个目录」，而是「转发到某个前缀」。

### 3.1 配置

```ts
export interface Config {
  target: string
  isolateOrigin?: boolean
  requestHeaders?: Record<string, string>
}
```

`target` 允许三种形态：

| 形态 | 含义 | 例 |
| --- | --- | --- |
| 空串 | 匹配所有 HTTP URL | `''` |
| scheme | 该 scheme 的所有 URL | `https:` |
| 以 `/` 结尾的 URL | 该 URL 前缀下的所有 URL | `https://example.com/api/` |

校验正则：`/^(?:https?:(?:\/\/[^/?#]+(?:\/[^?#]*)?\/)?)?$/i`——只允许 `http`/`https`、必须以 `/` 结尾或只有 scheme。

转发规则：`target + (请求路径去掉路由前缀) + query`。

```ts
function locate(req: Request, protocols: string[]) {
  const path = req.path.slice(prefix.length).replace(/^\//, '')
  let url: URL
  try {
    url = new URL(config.target + path + req.url.slice(req.path.length))
  } catch { return }
  if (contains(url) && protocols.includes(url.protocol)) return url
}
```

### 3.2 越界防护

```ts
const decode = (url: URL) => new URL(url.origin + url.pathname.replace(/%2f|%5c/gi, '/')).href
const target = new URL(config.target)
const decoded = decode(target)
contains = url => url.href.startsWith(target.href) && decode(url).startsWith(decoded)
```

双重比较：原始 href 前缀 + 把 `%2F`/`%5C` 解码后的前缀。这样 `..%2F..` 这类「上游会先解码再规范化」的逃逸也被挡住。

### 3.3 请求头映射

`requestHeaders` 把上游头名映射到唯一来源头名。规则：

| 规则 | 说明 |
| --- | --- |
| 大小写 | 名称大小写不敏感，内部统一小写 |
| 来源 | 被消费，透传时跳过 |
| 来源缺失 | 该上游头省略 |
| 保留头 | hop-by-hop 全集 + `host` + `expect` 不可映射 |
| WS 额外保留 | `sec-websocket-key`/`-version`/`-extensions`/`-accept`、`content-length` |
| 与 `isolateOrigin` 冲突 | 映射 `cookie` 直接报错 |

验证失败时抛 `TypeError`，不是静默忽略。

### 3.4 hop-by-hop 与 `Connection` 展开

```ts
function blockedHeaders(incoming: Headers, reserved: Iterable<string>) {
  const blocked = new Set(reserved)
  for (const field of incoming.get('connection')?.split(',') ?? []) {
    blocked.add(field.trim().toLowerCase())
  }
  return blocked
}
```

`Connection: x-foo` 会让 `x-foo` 也被视为逐跳头。这是 RFC 7230 的要求，基线没有做。

### 3.5 `isolateOrigin`

开启后：

| 侧 | 行为 |
| --- | --- |
| 请求 | 过滤入站 `Cookie` |
| 响应 | 删除 `Set-Cookie`、`Clear-Site-Data` |
| 响应 | 追加 `Content-Security-Policy: sandbox`、`X-Content-Type-Options: nosniff` |

用途：把上游文档放进不透明源，防止读到本站在同源下的存储。

### 3.6 服务被删除

跟进后 `ServerProxy` 服务与 `publicUrl` 都没了，`name` 也不再是服务：

```ts
// 基线存在，跟进后删除
declare module '@cordisjs/server' { interface Server { proxy: ServerProxy } }
ctx.reflect.provide('server.proxy', { get publicUrl() { return ctx.server.publicUrl } })
```

**这是对下游的可观测破坏**——任何 `ctx.server.proxy.publicUrl` 的调用都要改。

### 3.7 WS 转发

```ts
const wsUrl = locate(req, ['http:', 'https:', 'ws:', 'wss:'])
wsUrl.protocol = wsUrl.protocol.replace(/^http/, 'ws')
const headers = forwardRequestHeaders(req.headers, wsHandshakeHeaders)
const upstream = ctx.std.http.ws(wsUrl.href, { headers })
```

`wsHandshakeHeaders` = `reservedRequestHeaders` + `sec-websocket-key`/`-version`/`-extensions`/`-accept` + `content-length`。

## 4. server/acl 与 server/echo

两者本轮只有 `std:server` 改名与测试调整，无语义变化：

```ts
export const inject = ['std:server']       // echo、acl
ctx.std.server.post('', ...)               // echo
```

`acl` 的测试从 26 个（每个覆盖 HTTP 与 WS）重组，但断言内容未变。

## 5. 对 kardia 的对照

### 5.1 server core

| 上游修复 | kardia 现状 | 结论 |
| --- | --- | --- |
| 畸形百分号 → 400 | `pattern.rs:96-108` 检测到非法 `%` 时 `Err("invalid URI parameter escape")`，由 `dispatch` 转成 500 | **不等价**：应回 400，且应「该路由不匹配」而非整请求失败 |
| 查询串含 `?` | `request.rs:29-32` 用 `url.split('?')` 且只取第二段 | **有缺陷**，与上游基线同错 |
| IPv6 主机括号 | `server.rs:120-135` 直接用 `host` 拼 URL | **有缺陷** |
| 表单体只编码一次 | `form.rs:73-99` 每次生成新 boundary；`response.rs:141-150` `write_typed` 用 `or_insert` 保留旧 content-type | **有缺陷**：第二次设置表单体会保留上一次的 boundary |
| `std:server` | `"server"` | 小 |
| `_end` 私有化 | Rust 无对应 | 不适用 |

`form.rs` 那条值得展开：`write_typed()`（`response.rs:141-150`）用 `entry().or_insert(...)`，即「已有 content-type 就不覆盖」。如果插件先 `set_form_data(a)` 再 `set_form_data(b)`，第二次的 boundary 换了但头还指向第一次的 boundary，客户端解析必然失败。上游用一个 `_formType` 精确判断「这个 content-type 是我上次生成的吗」，是正确做法。

### 5.2 server static

| 上游修复 | kardia 现状 | 结论 |
| --- | --- | --- |
| 目录先于扩展名 | `serve.rs:91-110` `try_file()` 先 `read()`，非 Response 就进扩展名循环；`IsADirectory` 重定向在外层 `:52` 才判断 | **有缺陷**：目录会先被扩展名循环吃掉 |
| `std:fs` 拦截 | `serve.rs:28` `ctx.intercept("fs", ...)` | 随 `std:fs` 一起改 |

具体表现：访问 `/guide`，磁盘上有 `guide/` 目录同时有 `guide.html`，当前 kardia 会返回 `guide.html` 内容而不是 301 到 `/guide/`。上游 parity 测试已覆盖「带扩展名的目录且不重定向」这一组合。

### 5.3 server proxy

| 上游变更 | kardia 现状 | 结论 |
| --- | --- | --- |
| `target` 变前缀 | `plugin.rs:50-52` `Url::parse` 后强制补 `/`——即「目录」语义 | **语义冲突**，必须重写配置与 `locate` |
| `isolateOrigin` | 无 | 缺失 |
| `requestHeaders` 映射 | 无 | 缺失 |
| hop-by-hop + `Connection` 展开 | 无（透传整个 `HeaderMap`，只去掉少数几个） | 缺失 |
| 双重越界比较（`%2F` 解码后） | 有 `SEGMENT` 编码表 + `locate` 逐段重编码 | 思路不同，需按上游重写 |
| 删除 `ServerProxy` 服务 | `ServerProxy` 存在，`public_url()` 返回 `ctx.get::<Server>()?.public_url()` | 若跟进则删服务；**这会破坏下游** |

`ServerProxy.publicUrl` 在本项目里有一个实际用途：给下游应用拼接对外 URL。上游删掉它的理由是「proxy 只是转发，没有独立的对外 URL 概念」——理由成立，但删除动作应作为独立的破坏性变更公告，不能混在「前缀化」里悄悄发生。

### 5.4 优先级建议

| 项 | 优先级 | 理由 |
| --- | --- | --- |
| 查询串含 `?` | **P0** | 三行修复，现存错误行为 |
| 表单体 content-type | **P0** | 现存错误行为，产生不可解析响应 |
| IPv6 主机括号 | **P0** | 三行修复 |
| static 目录顺序 | **P1** | 需调整 `try_file` 与重定向关系，有测试 |
| 畸形百分号 → 400 | **P1** | 同时改「路由不匹配」语义，影响面稍大 |
| proxy 前缀化 | **P1** | 破坏性，需迁移配置并新增头映射 |
| `isolateOrigin` | **P2** | 新能力，可与前缀化一起做 |
| 删除 `ServerProxy` | **P3** | 破坏下游，需单独沟通 |

## 核对命令

```bash
cd cordis
git show 65edc8f -- packages/server/core/src/index.ts
git show a15b178 -- packages/server/core/src/body.ts
git show 418a010 -- packages/server/core/src/index.ts
git show 127e57e -- packages/server/core/src/body.ts
git show 93bc78a -- packages/server/static/src/index.ts
git show 7e4387d -- packages/server/proxy/src/index.ts
cd ..
grep -n "split('?'" crates/kardia-server/src/request.rs
grep -n "try_file\|IsADirectory" crates/kardia-server-static/src/serve.rs
```
