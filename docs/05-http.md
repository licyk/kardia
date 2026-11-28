# 05 http：noProxy、event-stream 与方法归一

涉及提交：`4fc11da`（`noProxy`）、`bea04de`（event-stream）、`e51673d`（方法大写）、`b1b8a44`（schema 描述）、`279ac22`（`std:http`）。

改动文件：`packages/http/src/{index,event-stream}.ts` + 测试，5 个文件 +391/−31。

## 1. `noProxy`

### 1.1 配置

```ts
export interface Http.Config extends Intercept {
  baseUrl?: string
  headers?: Dict
  timeout?: number
  proxyAgent?: string
  noProxy?: string[]        // 新增
}
```

`noProxy` 允许的匹配写法：

| 写法 | 匹配 |
| --- | --- |
| `*` | 所有主机 |
| `localhost` | `localhost` 及其子域 |
| `.example.com` | `.example.com` 及其子域 |
| `127.0.0.1:8080` | 仅该端口 |
| `[::1]` / `::1` | IPv6（无括号时自动补） |
| 尾随 `.` | 被剥掉后再比较（FQDN 写法） |

### 1.2 实现

```ts
const defaultPorts = { 'http:': '80', 'https:': '443', 'ws:': '80', 'wss:': '443' }

function bypassProxy(url: URL, noProxy: string[] = []) {
  const hostname = url.hostname.replace(/\.$/, '')
  const port = +(url.port || defaultPorts[url.protocol])
  return noProxy.some((entry) => {
    if (entry === '*') return true
    // 多个冒号且无方括号 → IPv6，补括号
    if (entry.indexOf(':') !== entry.lastIndexOf(':') && !entry.startsWith('[')) entry = `[${entry}]`
    const capture = /^(\[[\da-f:.]+\]|[\w.*\-\u0080-\uffff]+)(?::(\d+))?$/i.exec(entry)
    if (!capture || capture[2] && +capture[2] !== port) return false
    let domain: string
    try {
      domain = new URL(`http://${capture[1]}`).hostname.replace(/^\*?\./, '').replace(/\.$/, '')
    } catch { return false }
    return hostname === domain || hostname.endsWith(`.${domain}`)
  })
}
```

注意点：
- `noProxy` 只在**没有显式给 `proxyAgent`** 时生效（`fetch` 路径里先判 `config.proxyAgent`）。
- `noProxy` 是 `Http.Config` 的一部分，因此可以通过 `ctx.intercept('std:http', { noProxy: [...] })` 做上下文级覆盖。
- 匹配用的是**子域后缀**，不是 glob：`example.com` 匹配 `api.example.com`，但 `badexample.com` 不匹配（因为要求 `.${domain}` 前缀）。

## 2. event-stream

### 2.1 解码类型

```ts
export namespace Http {
  export interface ResponseTypes {
    json: any
    text: string
    stream: ReadableStream
    'event-stream': ReadableStream<ServerSentEvent>   // 新增
    blob: Blob
    formdata: FormData
    arraybuffer: ArrayBuffer
    headers: Headers
  }
}
```

构造函数里注册：`this.decoder('event-stream', (raw) => parseEventStream(raw.body))`。

### 2.2 `parseEventStream` 语义

按 WHATWG HTML 标准实现：

```ts
export interface ServerSentEvent {
  event: string   // 无 event 字段时为 'message'
  data: string    // 多行 data 用 \n 连接
  id: string      // 由本条或之前某条的 id 字段设置
  retry?: number  // 本条之前最后一个合法 retry 字段
}
```

边界处理（这些是容易做错的地方）：

| 情况 | 行为 |
| --- | --- |
| 以 `:` 开头的行 | 注释，忽略 |
| 行内无冒号 | 整行是字段名，值为空串 |
| 值以单个空格开头 | 去掉那一个空格 |
| 空行 | 结束当前事件并入队；`hasData` 为假则丢弃 |
| `id` 值含 `\0` | 忽略该字段 |
| `retry` 非纯数字 | 忽略 |
| 正文结束仍未遇到空行 | 丢弃该未终止事件 |
| BOM | `TextDecoder` 自动剥离 |
| `\r\n` 跨 chunk 分裂 | `skipLF` 标记避免把 `\n` 当新行 |
| 多字节字符跨 chunk | `decoder.decode(bytes, { stream: true })` 保留不完整字符 |

取消返回的流会取消底层 body。

## 3. 方法大写归一

```ts
const normalizedMethods = new Set(['DELETE', 'GET', 'HEAD', 'OPTIONS', 'POST', 'PUT'])

function normalizeMethod(method: string) {
  const upper = method.toUpperCase()
  return normalizedMethods.has(upper) ? upper : method
}
```

便捷方法改为发送大写：

```ts
defineProperty(Http.prototype, method, async function (url, config) {
  const response = await this(url, { method: method.toUpperCase(), validateStatus, ...config })
  return this._decode(response)
})
```

目的：`http/fetch` 监听者看到的方法与真正发出的方法一致。之前 `http.get()` 内部发的是 `'get'`，监听者据此判断会出错。

**注意**：这只归一化**六个标准方法**，自定义方法（如 `PROPFIND`）保持原样——符合 fetch 规范（`DELETE`/`GET`/`HEAD`/`OPTIONS`/`POST`/`PUT` 之外的字节大小写敏感）。

## 4. 对 kardia 的对照

| 上游能力 | kardia 现状 | 差距 |
| --- | --- | --- |
| `noProxy` 配置 | `HttpConfig` 只有 `proxy_agent`（`crates/kardia-http/src/config.rs:14`） | **缺失**，需新增 |
| event-stream 解码 | 无 SSE 类型；`grep` 全仓无 `event-stream`/`ServerSentEvent` | **缺失** |
| 六方法大写归一 | `transport.rs:6` 已 `to_ascii_uppercase()` 用于校验，但请求本身用 `request.method.clone()`（已是 `http::Method`，构造时归一） | **已具备**（Rust 的 `Method` 类型本身大小写敏感但标准方法由常量构造） |
| `std:http` 键 | `"http"` | 小 |
| `decoder()` 注册表 | `Http` 只有 `active_operations()`；解码在 `decode.rs` 以固定枚举实现 | 结构差异 |
| `data:` / `file:` URL | 无 | 已知缺口（parity README 已声明不追求完整 decoder 生态） |
| SOCKS | `kardia-http` 无；上游有独立 `http-socks` 包 | 不跟进 |

### 4.1 `noProxy` 的推荐实现

kardia 用 reqwest，代理是 `reqwest::Proxy`。`no_proxy` 有两种落法：

1. **交给 reqwest**：`reqwest::NoProxy` 已实现环境变量与 `NO_PROXY` 语义，但它读的是进程环境，不是配置。
2. **自己判断**：在 `socket.rs` 构造 client 前，先按上游算法判断目标是否命中 `no_proxy`，命中就不 `proxy()`。

推荐 2，因为：
- 配置必须可 per-context 覆盖（`HttpConfig` 已经是 intercept 可合并的结构）。
- 上游语义（子域后缀匹配、端口限定、IPv6 补括号）与 reqwest 的 `NoProxy` 有细微差异，自己实现才能对齐对照测试。

算法可直接移植 `bypassProxy()`，Rust 侧用 `url::Url` 的 `host_str()`/`port_or_known_default()`。

### 4.2 event-stream 的推荐实现

Rust 侧不需要 `ReadableStream`，建议：

```rust
pub struct ServerSentEvent { pub event: String, pub data: String, pub id: String, pub retry: Option<u64> }

/// 增量解析器：喂 chunk，产出已完成的事件。
pub struct EventStreamParser { /* pending line buffer, event/data/id/retry, skip_lf */ }
impl EventStreamParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<ServerSentEvent> { ... }
}
```

跨 chunk 的 UTF-8 用 `encoding_rs` 的增量解码或自己保留尾部不完整字节；`\r\n` 分裂用 `skip_lf`。测试应覆盖 `04`/`05` 文档里列的全部边界（尤其「未终止事件丢弃」「`id` 含 NUL 忽略」）。

由于 kardia 的 http 目前是 reqwest + 显式 `Body`，SSE 更适合作为**独立解码器**而不是响应类型枚举的一员——见 `11-implementation-plan.md` P2-3。

## 核对命令

```bash
cd cordis
git show 4fc11da -- packages/http/src/index.ts
git show bea04de -- packages/http/src/event-stream.ts
git show e51673d -- packages/http/src/index.ts
cd ..
grep -n "proxy_agent" crates/kardia-http/src/config.rs
```
