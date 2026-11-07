# @kardiajs/core

Plugins, revertible effects, lifecycle and reactive services for TypeScript: a port of [kardia](https://github.com/licyk/kardia)'s core (itself a port of [Cordis](https://github.com/cordiverse/cordis)). No dependencies; runs in browsers and Node.

```sh
npm install @kardiajs/core
```

```ts
import { Context, definePlugin } from "@kardiajs/core";

const greeter = definePlugin({
  name: "greeter",
  inject: ["store"],
  apply(ctx) {
    const store = ctx.get("store"); // only declared services: anything else is a type error
    ctx.on("internal/service", (key) => ctx.logger().info(`service ${key} changed`));
    return () => console.log("stopped"); // runs when the plugin stops
  },
});

const root = new Context();
const fiber = root.plugin(greeter); // waits for `store`
root.provide("store", new Map()); // now it starts
await fiber.settled();
await fiber.dispose();
```

- **Plugins** are functions, classes or objects with `apply`, with optional `name`, `inject`, `provide` and `Config` (any Standard Schema validator).
- **Services** are keyed by string and typed by declaration merging on `Services`. A plugin starts once its dependencies are there, restarts when one is replaced, and stops when one leaves; a provider keeps running until its consumers have cleaned up. A `Service` subclass sees the plugin that called it as `this.caller`.
- **Effects** (`ctx.effect`, `ctx.onDispose`, listeners, provisions, child plugins) are reverted when their plugin stops.
- **Lifecycle:** `fiber.settled()`, `update(config)`, `restart()`, `dispose()`, `ctx.onUpdate(...)`, and `ctx.registry.replace(old, new)` for hot replacement.
- **Events**, **isolation** (`ctx.isolate`), **interception** (`ctx.intercept`) and a **logger** with exporters.

Guide: [TypeScript and Tauri](https://kardia-dev.netlify.app/docs/guide-typescript). License: GPL-3.0-only.
