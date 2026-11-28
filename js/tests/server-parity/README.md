# Server reference and parity tests

Reference: Cordis `6ad12cf2b39e7a22fe54b2f4568e948ecc778e97` (MIT; see the repository NOTICE).

From `js/`, with Node 24, Bun and Corepack available:

```sh
bun install --frozen-lockfile
bun run server:prepare
bun run server:inventory --check
bun run server:mime --check
bun run check
cargo build --manifest-path ../Cargo.toml -p kardia --example server-reference
bun run test:server:reference
```

`server:prepare` checks out the exact commit into the ignored `cordis/` directory if absent, verifies an existing checkout instead of resetting it, installs its immutable Yarn lockfile, builds it, and runs the original four Server suites (156 tests: core 67, acl 15, proxy 47, static 27) and the FS suite (162 tests; one Windows-only test is skipped elsewhere). `server:inventory` regenerates `inventory.json` from those suites. The inventory uses the pinned TypeScript parser to expand parameterized declarations into cases; it does not establish Rust coverage or passing status.

The additional reference tests launch the actual upstream service in a child process. HTTP and WebSocket clients use real loopback sockets. Readiness and shutdown are explicit, with deadlines. The adapters register fixture handlers; they do not reproduce routing, proxying, MIME detection or ACL logic. The initial 22 cases cover HTTP matching, HEAD/OPTIONS, decoding, response ownership, replay of inspected streams, Echo and WebSocket proxy text/binary frames.

Rust parity is added alongside each implementation stage. A passing reference test alone is not parity. Normalization must be restricted to environment-dependent values; semantic headers, body bytes and WS messages must remain comparable.

## Implementation decisions

- Hyper 1.12 (MIT, MSRV 1.63) provides HTTP/1 transport. Kardia owns routing and lifecycle so framework defaults cannot add implicit HEAD or change Allow responses. [Server API](https://hyper.rs/guides/1/server/hello-world/).
- tokio-tungstenite 0.30 (MIT, MSRV 1.85) provides WS streams and protocol handling. [API](https://docs.rs/tokio-tungstenite/0.30.0/tokio_tungstenite/).
- Reqwest 0.13.5 (MIT/Apache-2.0, MSRV 1.85) supplies the HTTP client transport, streaming and decompression. Client settings must match the actual Cordis HTTP service rather than Reqwest defaults. [Builder API](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html).
- Recovery defaults to remaining failed. The host can select automatic retry or a custom handler. Supervision belongs outside the failed instance; it uses the existing lifecycle recovery operation and never resets the root to repair one plugin. All modes must keep unrelated plugins running.

The original suite had 22 upstream supplement cases, 51 HTTP comparisons, 5 WS/Echo comparisons, 26 ACL cases (each tests HTTP and WS), and 29 Static cases. The standalone `Server reference` workflow (`.github/workflows/server-reference.yml`) builds the Rust adapter and runs the same suite on Linux, Windows and macOS. Start it with **Run workflow** in GitHub Actions; it only supports `workflow_dispatch` and does not run automatically on pushes, pull requests or releases. HTTP client and Proxy comparisons are described below; native acceptance is tracked in the execution record.

Static compares 26 configuration sets across 15 paths, native FS error codes, distinct error-page resolution, and the complete file-type 20.5.0 fixture directory. `server:prepare` also fetches the fixture repository at `7fc6d9ff7cc01987aa73006757792a5ca198023a` into `cordis/.file-type-reference`. All 254 fixtures are served without extensions, plus generic ASF and HEIC-sequence supplements; the test asserts coverage of all 171 upstream supported MIME values. Every response compares status, reason phrase, semantic headers and a SHA-256 digest of the complete body. No MIME differences are normalized away.

`server:mime` generates Rust signatures and 1,180 extension mappings from the installed fixed upstream dependencies. Complex container readers are implemented in Rust; the source hash guards regeneration against an unaudited upstream change. The generator uses the upstream TypeScript parser and existing rustfmt tooling. Content detection owns an independent, cancellable FS stream; no production Node process is required. picomatch-rs 0.1.0 supplies glob compilation, with the unspecified upstream POSIX option reproduced for literal `!` in character classes and tested over shared configuration samples. NOTICE preserves the adapted sources' MIT notices.

Rust tests cover Static behavior, first-read/stream errors, provider panics, client cancellation and three repeated failures under each recovery policy while an independent plugin continues handling HTTP requests. These are distinct from upstream protocol parity and from final native CI acceptance.

The HTTP client adds 28 real comparisons, including trusted HTTPS/WSS with a generated local CA, HTTP status/reason/body preservation, five compression formats, redirects with replayable and streaming bodies, method normalization, request header behavior, configuration interception, timeouts and WS subprotocols. Rust additionally rejects unknown certificates and hostname mismatches, verifies cancellation, and repeats provider failure under all three recovery policies while an independent plugin continues serving. TLS uses rustls with platform certificate verification and optional additional roots; verification is never disabled. Decompression is explicit so decoded bytes retain the original raw response headers until Proxy removes them.

This auxiliary client exposes the raw HTTP(S)/WS(S) path needed by Proxy, caller-owned operations, and HTTP proxy configuration. `data:` URLs are answered from their own content, without a transport; `file:` URLs are deliberately left to `std:fs`, which serves them through the provider mounts, and SOCKS is out of scope. WebSocket frame transport does not offer permessage-deflate; application text/binary behavior is tested against the real upstream.

The original Proxy suite added 25 comparisons: prefixed/encoded/root paths, repeated queries, six methods, request bodies and headers, gzip, redirects, status codes, cookies, public URL, WS text/binary/subprotocols, both close directions, transport errors and unavailable targets. Nine Rust tests migrate the original declarations; two additional tests exercise upstream availability and three repeated Proxy/HTTP-provider failures under each policy while healthy HTTP and the same healthy WS connection keep progressing. An upstream connection failure leaves the Proxy active; a failed WS connection is never accepted downstream.

Actual upstream observations override assumptions: adopting a returned Response drops its custom reason phrase; Headers.set retains only the last Set-Cookie; server.proxy.publicUrl uses the providing plugin context, including its mount prefix, rather than a later consumer's intercept. The parity tests retain these behaviors. The server uses Node's reason spelling, including 418 and 509.

The final HTTP audit adds stateful g/y regular expressions, Unicode case folding, all 1,180 MIME extension negotiations, ECMAScript JSON numeric/property formatting, and 15 valid/malformed WS handshakes. That audit brought the suite to 201 tests. The current directory/path update adds 10 Proxy comparisons, two Static comparisons and 11 FS comparisons (13 on Windows), for 224 tests on Linux/macOS and 226 on Windows. FS runs both actual services through the fixture hosts, including directory interception, invalid bases and native Windows drive conversion. HTTP/WS escape tests assert that the upstream target was not contacted. Rust additionally cancels and awaits pending route handlers, response sources, middleware, route hooks and upgrade hooks when their owner is disposed. Sources remain streamed with bounded buffering.

Rust APIs use owned UTF-8 strings, serde JSON values and typed callbacks rather than JS prototypes, callable objects or arbitrary UTF-16 strings. JSON response encoding applies ECMAScript number formatting and array-index property ordering; applications requiring integers beyond JS Number precision should transmit them as strings. Transport and application behavior is checked against the pinned runtime; this does not assert a universal equivalence between Rust and JavaScript value systems.

Returned-Response adoption is shared by ordinary routes and Proxy, including the last-Set-Cookie rule; a dedicated real comparison guards this behavior.
