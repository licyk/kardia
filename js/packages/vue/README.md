# @kardiajs/vue

Vue integration for [`@kardiajs/core`](https://www.npmjs.com/package/@kardiajs/core): extensions contribute components, views, settings sections, commands and routes to a Vue app, as effects of their plugins, so whatever a plugin contributed leaves when it stops.

```sh
npm install @kardiajs/core @kardiajs/vue vue
```

```ts
import { Context } from "@kardiajs/core";
import { type ComponentContribution, Contributions, createKardia, defineContributions } from "@kardiajs/vue";
import { createApp } from "vue";

declare module "@kardiajs/core" {
  interface Services {
    "ui.panels": Contributions<ComponentContribution>;
  }
}

const root = new Context();
root.plugin(defineContributions("ui.panels"));
createApp(App).use(createKardia(root)).mount("#app");

// an extension
root.inject(["ui.panels"], (ctx) => {
  ctx.get("ui.panels").register({ id: "clock", component: ClockPanel, order: 10 });
});
```

```vue
<ExtensionSlot name="ui.panels" :props="{ compact: true }">
  <p>Nothing here yet.</p>
</ExtensionSlot>
```

| | |
|---|---|
| `createKardia(root)` | The Vue plugin: makes `root` every component's context |
| `Contributions<T>`, `defineContributions(key)` | A contribution point; `register(item)` is an effect of the calling plugin. Items are ordered by `order`, `id` is unique, `items` / `entries` are reactive |
| `<ExtensionSlot name>` | Renders a point's components, each with the context of the plugin that contributed it; `props`, a default slot for the empty case, an `item` slot. A contribution that throws is replaced by the `error` slot (`{ error, item, owner, retry }`), or by nothing, and reported to `@error` (`{ error, item, owner }`), or logged by its owner; the rest keep rendering and the owner keeps running |
| `useContext()`, `usePlugin(plugin, config)` | The component's context; a plugin that runs while the component lives |
| `useService(key)`, `useContributions(key)`, `useFiberState(fiber)` | Refs that follow a service, a point's items, a fiber's state |
| `addRoute(ctx, router, route)` | A vue-router route, removed when the plugin stops |

With [`@kardiajs/bridge`](https://www.npmjs.com/package/@kardiajs/bridge), an extension that injects `host.<name>` contributes its UI exactly while the Rust app exposes that service. Guide: [TypeScript and Tauri](https://kardia-dev.netlify.app/docs/guide-typescript#vue). Needs Vue 3.3 or later. License: GPL-3.0-only.
