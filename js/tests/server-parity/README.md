# Server reference and parity tests

Reference: Cordis `f6290dbbf1558cb3a870a58b1f97bacc0d3a9960` (MIT; see the repository NOTICE).

From `js/`, with Node 24, Bun and Corepack available:

```sh
bun install --frozen-lockfile
bun run server:prepare
bun run server:inventory --check
bun run check
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

Versions above were checked with `cargo info`; executable Rust transport and recovery proofs belong to P2/P3/P7 and remain pending until implemented. See the root execution plan for stage evidence and outstanding work.
