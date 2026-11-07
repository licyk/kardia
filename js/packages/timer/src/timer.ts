import { type Context, type EffectHandle, Service } from "@kardiajs/core";

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
  timeout(delay: number, callback: () => void): EffectHandle {
    const handle: EffectHandle = this.caller.effect((scope) => {
      const timer = setTimeout(() => {
        // revert the effect first, as the timer is spent
        void handle.dispose();
        callback();
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
  interval(delay: number, callback: () => void): EffectHandle {
    return this.caller.effect((scope) => {
      const timer = setInterval(callback, delay);
      scope.defer(() => clearInterval(timer));
    }, "ctx.interval()");
  }

  /**
   * Calls `callback` at most once per `delay` ms. A call inside the window is
   * postponed to its end, replacing an earlier postponed call, unless
   * `trailing` is false.
   */
  throttle<A extends unknown[]>(delay: number, callback: (...args: A) => void, options: ThrottleOptions = {}): Scheduled<A> {
    let lastCall = -Infinity;
    const execute = (args: A) => {
      lastCall = Date.now();
      callback(...args);
    };
    return this.schedule<A>("ctx.throttle()", (args) => {
      const remaining = delay - (Date.now() - lastCall);
      if (remaining <= 0) {
        execute(args);
      } else if (options.trailing !== false) {
        return setTimeout(() => execute(args), remaining);
      }
    });
  }

  /** Calls `callback` once `delay` ms have passed without another call. */
  debounce<A extends unknown[]>(delay: number, callback: (...args: A) => void): Scheduled<A> {
    return this.schedule<A>("ctx.debounce()", (args) => setTimeout(() => callback(...args), delay));
  }

  private schedule<A extends unknown[]>(
    label: string,
    trigger: (args: A) => ReturnType<typeof setTimeout> | undefined,
  ): Scheduled<A> {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;
    const handle = this.caller.effect((scope) => {
      scope.defer(() => {
        disposed = true;
        clearTimeout(timer);
      });
    }, label);
    const scheduled = (...args: A) => {
      if (disposed) return;
      clearTimeout(timer);
      timer = trigger(args);
    };
    return Object.assign(scheduled, { dispose: () => handle.dispose() });
  }
}
