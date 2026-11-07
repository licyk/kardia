# @kardiajs/timer

The `timer` service for [`@kardiajs/core`](https://www.npmjs.com/package/@kardiajs/core): every timer is an effect of the plugin that scheduled it, so it is cancelled when that plugin stops.

```ts
import { Context } from "@kardiajs/core";
import { Timer } from "@kardiajs/timer";

const root = new Context();
root.plugin(Timer);
root.inject(["timer"], async (ctx) => {
  const timer = ctx.get("timer");
  timer.interval(1000, () => console.log("tick"));
  const save = timer.debounce(300, (text: string) => console.log("save", text));
  save("a");
  save("ab"); // only this one runs
  await timer.sleep(500); // rejects with `Disposed` if the plugin stops first
});
```

`timeout`, `interval`, `sleep`, `throttle` (with or without a trailing call) and `debounce`, as in the Rust crate `kardia-timer`. License: GPL-3.0-only.
