# Server reference and parity tests

Reference: Cordis `f6290dbbf1558cb3a870a58b1f97bacc0d3a9960` (MIT; see the repository NOTICE).

From `js/`, with Node 24, Bun and Corepack available:

```sh
bun install --frozen-lockfile
bun run server:prepare
bun run server:inventory --check
bun run server:mime --check
bun run check
cargo build --manifest-path ../Cargo.toml -p kardia-server --example server-reference
bun run test:server:reference
```

`server:prepare` checks out the exact commit into the ignored `cordis/` directory if absent, verifies an existing checkout instead of resetting it, installs its immutable Yarn lockfile, builds it, and runs the original four Server suites (105 tests). `server:inventory` regenerates `inventory.json` from those suites. The inventory records source declarations, not Rust coverage or passing status.

The additional reference tests launch the actual upstream service in a child process. HTTP and WebSocket clients use real loopback sockets. Readiness and shutdown are explicit, with deadlines. The adapters register fixture handlers; they do not reproduce routing, proxying, MIME detection or ACL logic. The initial 22 cases cover HTTP matching, HEAD/OPTIONS, decoding, response ownership, replay of inspected streams, Echo and WebSocket proxy text/binary frames.

Rust parity is added alongside each implementation stage. A passing reference test alone is not parity. Normalization must be restricted to environment-dependent values; semantic headers, body bytes and WS messages must remain comparable.

## Implementation decisions

- Hyper 1.12 (MIT, MSRV 1.63) provides HTTP/1 transport. Kardia owns routing and lifecycle so framework defaults cannot add implicit HEAD or change Allow responses. [Server API](https://hyper.rs/guides/1/server/hello-world/).
- tokio-tungstenite 0.30 (MIT, MSRV 1.85) provides WS streams and protocol handling. [API](https://docs.rs/tokio-tungstenite/0.30.0/tokio_tungstenite/).
- Reqwest 0.13.5 (MIT/Apache-2.0, MSRV 1.85) supplies the HTTP client transport, streaming and decompression. Client settings must match the actual Cordis HTTP service rather than Reqwest defaults. [Builder API](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html).
- Recovery defaults to remaining failed. The host can select automatic retry or a custom handler. Supervision belongs outside the failed instance; it uses the existing lifecycle recovery operation and never resets the root to repair one plugin. All modes must keep unrelated plugins running.

The current suite has 22 upstream supplement cases, 42 HTTP comparisons, 5 WS/Echo comparisons, 26 ACL cases (each tests HTTP and WS), and 29 Static cases. The CI reference job builds the Rust adapter and runs the same suite on Linux, Windows and macOS. Proxy client parity and final integration acceptance remain pending; see the root execution plan.

Static compares 26 configuration sets across 15 paths, native FS error codes, distinct error-page resolution, and the complete file-type 20.5.0 fixture directory. `server:prepare` also fetches the fixture repository at `7fc6d9ff7cc01987aa73006757792a5ca198023a` into `cordis/.file-type-reference`. All 254 fixtures are served without extensions, plus generic ASF and HEIC-sequence supplements; the test asserts coverage of all 171 upstream supported MIME values. Every response compares status, reason phrase, semantic headers and a SHA-256 digest of the complete body. No MIME differences are normalized away.

`server:mime` generates Rust signatures and 1,180 extension mappings from the installed fixed upstream dependencies. Complex container readers are implemented in Rust; the source hash guards regeneration against an unaudited upstream change. The generator uses the upstream TypeScript parser and existing rustfmt tooling. Content detection owns an independent, cancellable FS stream; no production Node process is required. picomatch-rs 0.1.0 supplies glob compilation, with the unspecified upstream POSIX option reproduced for literal `!` in character classes and tested over shared configuration samples. NOTICE preserves the adapted sources' MIT notices.

Rust tests cover all 24 original Static declarations, first-read/stream errors, provider panics, client cancellation and three repeated failures under each recovery policy while an independent plugin continues handling HTTP requests. These are distinct from upstream protocol parity and from final native CI acceptance.

The HTTP client adds 28 real comparisons, including trusted HTTPS/WSS with a generated local CA, HTTP status/reason/body preservation, five compression formats, redirects with replayable and streaming bodies, method normalization, request header behavior, configuration interception, timeouts and WS subprotocols. Rust additionally rejects unknown certificates and hostname mismatches, verifies cancellation, and repeats provider failure under all three recovery policies while an independent plugin continues serving. TLS uses rustls with platform certificate verification and optional additional roots; verification is never disabled. Decompression is explicit so decoded bytes retain the original raw response headers until Proxy removes them.

This auxiliary client exposes the raw HTTP(S)/WS(S) path needed by Proxy, caller-owned operations, and HTTP proxy configuration. It does not claim the complete upstream decoder, file/data URL or SOCKS ecosystem. WebSocket frame transport does not offer permessage-deflate; application text/binary behavior is tested against the real upstream.

Proxy adds 25 comparisons: prefixed/encoded/root paths, repeated queries, six methods, request bodies and headers, gzip, redirects, status codes, cookies, public URL, WS text/binary/subprotocols, both close directions, transport errors and unavailable targets. Nine Rust tests migrate the original declarations; two additional tests exercise upstream availability and three repeated Proxy/HTTP-provider failures under each policy while healthy HTTP and the same healthy WS connection keep progressing. An upstream connection failure leaves the Proxy active; a failed WS connection is never accepted downstream.

Actual upstream observations override assumptions: adopting a returned Response drops its custom reason phrase; Headers.set retains only the last Set-Cookie; server.proxy.publicUrl uses the providing plugin context, including its mount prefix, rather than a later consumer's intercept. The parity tests retain these behaviors. The server uses Node's reason spelling, including 418 and 509.
