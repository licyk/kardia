# @kardiajs/bridge

The client side of the [kardia](https://github.com/licyk/kardia) bridge. It connects a [`@kardiajs/core`](https://www.npmjs.com/package/@kardiajs/core) app (for example the interface of a Tauri app) to a Rust app running the `kardia-bridge` crate.

```ts
import { Context } from "@kardiajs/core";
import { Bridge, type RemoteService } from "@kardiajs/bridge";
import { tauriBridge } from "@kardiajs/tauri";

declare module "@kardiajs/core" {
  interface Services {
    "host.agent": RemoteService<{ ping(args: string): string }>;
  }
}

const root = new Context();
root.plugin(Bridge, tauriBridge());

// runs exactly while the Rust app exposes `agent`, and restarts when it is exposed anew
root.inject(["host.agent"], async (ctx) => {
  console.log(await ctx.get("host.agent").call("ping", "hello"));
});

root.on("host/agent/ready", () => console.log("ready")); // a host event

// a service the Rust app can call, while this plugin runs
root.inject(["bridge"], (ctx) => {
  ctx.get("bridge").provide("ui.picker", { methods: { pick: async () => ["a.txt"] } });
});
```

- Host services appear as `host.<name>`, host events as `host/<name>`.
- Calls are asynchronous and reject with a `BridgeError` (`unavailable`, `timeout`, `invalid` or `failed`).
- The client reconnects when the host's bridge restarts, and provides its services again. `bridge.status` and the `bridge/status` event report the connection.
- The transport is pluggable (`Transport`): `@kardiajs/tauri` provides Tauri's.

Guide and protocol: [TypeScript and Tauri](https://kardia-dev.netlify.app/docs/guide-typescript). License: GPL-3.0-only.
