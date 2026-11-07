import { dispatch } from "./events.js";
import { reload, unload } from "./lifecycle.js";
import { Level, Logger, LoggerState } from "./logger.js";
import type { FiberId } from "./scope.js";
import { type Action, State } from "./state.js";

interface Waiter {
  predicate: (st: State) => boolean;
  resolve: () => void;
}

/** Shared state of one root context. */
export class Root {
  readonly state = new State();
  readonly logger = new LoggerState();
  private readonly waiters = new Set<Waiter>();

  /**
   * Runs one critical section, then wakes waiters and executes the actions it
   * queued. `f` must not call back into `run`.
   */
  run<R>(f: (st: State) => R): R {
    try {
      return f(this.state);
    } finally {
      const actions = this.state.actions;
      this.state.actions = [];
      this.wake();
      for (const action of actions) this.execute(action);
    }
  }

  /** A read-only critical section. */
  with<R>(f: (st: State) => R): R {
    return f(this.state);
  }

  /** Resolves once `predicate` holds; it is checked after every critical section. */
  waitUntil(predicate: (st: State) => boolean): Promise<void> {
    if (predicate(this.state)) return Promise.resolve();
    return new Promise((resolve) => this.waiters.add({ predicate, resolve }));
  }

  /** Logs an error as the fiber's plugin. */
  logInternal(fiber: FiberId, error: unknown, level: Level = Level.error) {
    new Logger(this, this.state.fiberName(fiber), fiber).logError(error, level);
  }

  private wake() {
    for (const waiter of [...this.waiters]) {
      if (waiter.predicate(this.state)) {
        this.waiters.delete(waiter);
        waiter.resolve();
      }
    }
  }

  private execute(action: Action) {
    switch (action.type) {
      case "reload":
        void reload(this, action.fiber);
        break;
      case "unload":
        void unload(this, action.fiber);
        break;
      case "emit":
        dispatch(this, action.name, action.args(this), action.filter);
        break;
      case "log":
        this.logInternal(action.fiber, action.error, action.level);
        break;
    }
  }
}
