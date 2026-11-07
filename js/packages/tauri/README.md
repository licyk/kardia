# @kardiajs/tauri

The Tauri transport of the [kardia](https://github.com/licyk/kardia) bridge, for [`@kardiajs/bridge`](https://www.npmjs.com/package/@kardiajs/bridge): every request is one Tauri command, and each session's messages arrive on a `Channel`.

```ts
import { Context } from "@kardiajs/core";
import { Bridge } from "@kardiajs/bridge";
import { tauriBridge } from "@kardiajs/tauri";

const root = new Context();
// the webview's label is the client label: a reloaded page replaces its own session
root.plugin(Bridge, tauriBridge({ command: "kardia_bridge" }));
```

On the Rust side, the command hands requests to `kardia_bridge::Bridge::handle`; the `kardia-bridge` crate docs show it in full. Needs `@tauri-apps/api` 2. License: GPL-3.0-only.
