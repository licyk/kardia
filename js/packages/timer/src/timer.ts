import { type Context, type EffectHandle, Service } from "@kardiajs/core";
import { Callbacks } from "#/callbacks";

declare module "@kardiajs/core" {
  interface Services {
    timer: Timer;
  }
}

/** The caller of a timer stopped before it fired. */
export class Disposed extends Error {
  override name = "Disposed";

  constructor() {
    super("Context has been disposed");
  }
}

/** A rate-limited callback. Calls after `dispose` are ignored. */
export type Scheduled<A extends unknown[]> = ((...args: A) => void) & { dispose(): Promise<void> };

export interface ThrottleOptions {
  /** Postpone a call inside the window to its end (default), or drop it. */
  trailing?: boolean;
}

/**
 * The `timer` service. Every timer is an effect of the plugin that scheduled
 * it, so it is cancelled when that plugin stops. Use it as a plugin:
 * `ctx.plugin(Timer)`, then inject `timer`.
 */
export class Timer extends Service {
  static readonly provide = "timer";

  constructor(ctx: Context) {
    super(ctx, "timer");
  }

  /** Calls `callback` once after `delay` ms, unless the effect is disposed first. */
  timeout(delay: number, callback: () => unknown): EffectHandle {
    const handle: EffectHandle = this.caller.effect((scope) => {
      const calls = new Callbacks(this.caller);
      scope.defer(() => calls.stop());
      const timer = setTimeout(() => {
        void calls.invoke(callback).then(() => handle.dispose()).catch(() => {});
      }, delay);
      scope.defer(() => clearTimeout(timer));
    }, "ctx.timeout()");
    return handle;
  }

  /** Resolves after `delay` ms; rejects with {@link Disposed} if the caller stops first. */
  sleep(delay: number): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      const handle = this.caller.effect((scope) => {
        const timer = setTimeout(() => {
          resolve();
          void handle.dispose();
        }, delay);
        scope.defer(() => {
          clearTimeout(timer);
          reject(new Disposed());
        });
      }, "ctx.timeout()");
    });
  }

  /** Calls `callback` every `delay` ms until the effect is disposed. */
  interval(delay: number, callback: () => unknown): EffectHandle {
    return this.caller.effect((scope) => {
      const calls = new Callbacks(this.caller);
      scope.defer(() => calls.stop());
      const timer = setInterval(() => { void calls.invoke(callback); }, delay);
      scope.defer(() => clearInterval(timer));
    }, "ctx.interval()");
  }

  /**
   * Calls `callback` at most once per `delay` ms. A call inside the window is
   * postponed to its end, replacing an earlier postponed call, unless
   * `trailing` is false.
   */
  throttle<A extends unknown[]>(delay: number, callback: (...args: A) => unknown, options: ThrottleOptions = {}): Scheduled<A> {
    let lastCall = -Infinity;
    return this.schedule<A>("ctx.throttle()", callback, (execute) => {
      const remaining = delay - (Date.now() - lastCall);
      const run = () => {
        lastCall = Date.now();
        execute();
      };
      if (remaining <= 0) {
        run();
      } else if (options.trailing !== false) {
        return setTimeout(run, remaining);
      }
    });
  }

  /** Calls `callback` once `delay` ms have passed without another call. */
  debounce<A extends unknown[]>(delay: number, callback: (...args: A) => unknown): Scheduled<A> {
    return this.schedule<A>("ctx.debounce()", callback, (execute) => setTimeout(execute, delay));
  }

  private schedule<A extends unknown[]>(
    label: string,
    callback: (...args: A) => unknown,
    trigger: (execute: () => void) => ReturnType<typeof setTimeout> | undefined,
  ): Scheduled<A> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    const calls = new Callbacks(this.caller);
    const handle = this.caller.effect((scope) => {
      scope.defer(() => calls.stop());
      scope.defer(() => {
        disposed = true;
        clearTimeout(timer);
      });
    }, label);
    const scheduled = (...args: A) => {
      if (disposed) return;
      clearTimeout(timer);
      timer = trigger(() => { void calls.invoke(() => callback(...args)); });
    };
    return Object.assign(scheduled, { dispose: () => handle.dispose() });
  }
}
