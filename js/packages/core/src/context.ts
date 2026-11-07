import { type Disposer, EffectCell, EffectHandle, EffectScope, isPromiseLike, runSequence } from "./effect.js";
import { Interrupted } from "./errors.js";
import { type EventArgs, type EventName, type Listener, bail, dispatch, listen, parallel, serial } from "./events.js";
import { type Fiber, fiberHandle } from "./fiber.js";
import { liveImpl } from "./lifecycle.js";
import { type Exporter, hyphenate, Logger, type LoggerIntercept, type Message } from "./logger.js";
import type { ConfigOf, InjectSpec, Plugin } from "./plugin.js";
import { insertFiber, Registry } from "./registry.js";
import { Root } from "./root.js";
import { type FiberId, type Realm, ROOT_FIBER, Scope } from "./scope.js";
import { access, bind, declaredMeta, isBindable, type ServiceOf, withdraw } from "./service.js";
import type { Check, ImplId } from "./state.js";

/** What an effect body may return: nothing, an inverse, or a promise of either. */
export type EffectResult = void | Disposer | PromiseLike<void | Disposer>;

export interface ProvideOptions {
  /** An extra availability predicate, given each consumer's interception metadata for the key. */
  check?: Check;
}

export interface ListenOptions {
  /** Run before the listeners registered so far. */
  prepend?: boolean;
}

/**
 * A cheap handle to a position in the runtime: a root, the fiber that owns
 * effects created through it, and its derivation scope (isolations and
 * interceptions). `new Context()` creates a root.
 *
 * `D` lists the services the context may access with {@link Context.get}; a
 * plugin defined with `definePlugin` gets a context typed by its `inject`.
 */
export class Context<D extends string = string> {
  /** @internal */ readonly _root: Root;
  /** @internal */ readonly _fiber: FiberId;
  /** @internal */ readonly _scope: Scope;
  private views: Map<ImplId, unknown> | undefined;

  constructor();
  /** @internal */
  constructor(root: Root, fiber: FiberId, scope: Scope);
  constructor(root?: Root, fiber?: FiberId, scope?: Scope) {
    this._root = root ?? new Root();
    this._fiber = fiber ?? ROOT_FIBER;
    this._scope = scope ?? this._root.state.fiber(ROOT_FIBER)!.scope;
  }

  /** The fiber that owns effects created through this context. */
  get fiber(): Fiber {
    return fiberHandle(this._root, this._fiber, this._root.state.fiber(this._fiber)?.scope ?? this._scope);
  }

  /** The root context. */
  get root(): Context {
    return new Context(this._root, ROOT_FIBER, this._root.state.fiber(ROOT_FIBER)!.scope);
  }

  get isRoot() {
    return this._fiber === ROOT_FIBER;
  }

  /** Whether both contexts belong to the same root. */
  sameRoot(other: Context<any>) {
    return this._root === other._root;
  }

  get registry() {
    return new Registry(this._root);
  }

  toString() {
    return `Context <${this._root.state.fiberName(this._fiber)}>`;
  }

  [Symbol.for("nodejs.util.inspect.custom")]() {
    return this.toString();
  }

  // Scope

  /** Derives a context in which `key` lives in its own realm: a fresh one, or `realm` to share it. */
  isolate(key: string, realm: Realm = Symbol(key)): Context<D> {
    return new Context(this._root, this._fiber, this._scope.child({ type: "isolate", key, realm }));
  }

  /** Derives a context carrying interception metadata for `key`. */
  intercept(key: string, meta: unknown): Context<D> {
    return new Context(this._root, this._fiber, this._scope.child({ type: "intercept", key, meta }));
  }

  /** The realm `key` resolves to from this context, once it has one. */
  realm(key: string): Realm | undefined {
    return this._root.with((st) => st.realmOf(this._scope, key));
  }

  /** Interception metadata for `key`: what the declaring plugin's `inject` says, then the contexts from outermost to this one. */
  intercepts(key: string): unknown[] {
    const declared = this._root.with((st) => declaredMeta(st, this, key));
    return [...(declared === undefined ? [] : [declared]), ...this._scope.intercepts(key)];
  }

  /** Interception metadata for `key`, merged key by key with later values winning. */
  interceptMeta<T extends object = Record<string, unknown>>(key: string): Partial<T> {
    return Object.assign({}, ...this.intercepts(key).filter((value) => typeof value === "object" && value !== null));
  }

  // Plugins

  /** Instantiates a plugin as a child fiber. The instantiation is an effect of this context's fiber. */
  plugin<P extends Plugin>(plugin: P, config?: ConfigOf<P>): Fiber {
    return this._root.run((st) => insertFiber(this._root, st, this, plugin, config));
  }

  /** Instantiates an anonymous plugin that runs while the given services are available. */
  inject<const K extends string>(inject: InjectSpec<K>, apply: (ctx: Context<K>) => unknown): Fiber {
    return this.plugin({ inject, apply: (ctx: Context<any>) => apply(ctx) });
  }

  // Services

  /** Provides `value` under `key`, as an effect of this context's fiber. */
  provide(key: string, value: unknown, options: ProvideOptions = {}): EffectHandle {
    const root = this._root;
    return this.effect((scope) => {
      const id = root.run((st) => bind(st, this, key, value, options.check));
      scope.defer(withdraw(root, id));
    }, `ctx.provide(${JSON.stringify(key)})`);
  }

  /**
   * Checked access: the service this context's plugin committed to. Throws an
   * `AccessError` for a service it did not declare, or one that is inactive.
   * A {@link Service} comes back as a view whose `caller` is this context.
   */
  get<K extends D>(key: K): ServiceOf<K> {
    const id = this._root.with((st) => access(st, this, key));
    return this.view(id) as ServiceOf<K>;
  }

  /** Reflective access to the live store: the current binding, if its provider is active. Never throws. */
  live<K extends string>(key: K): ServiceOf<K> | undefined {
    const id = this._root.with((st) => liveImpl(st, this._scope, key, true));
    return id === undefined ? undefined : (this.view(id) as ServiceOf<K>);
  }

  private view(id: ImplId): unknown {
    const value = this._root.state.impls.get(id)!.value;
    if (!isBindable(value)) return value;
    this.views ??= new Map();
    if (!this.views.has(id)) this.views.set(id, value[Symbol.for("kardia.bind") as keyof typeof value](this));
    return this.views.get(id);
  }

  // Effects

  /**
   * Runs an effect body. Inverses collected with `scope.defer`, or returned
   * by the body, are disposed when the effect is (newest first), at the
   * latest when this context's fiber stops.
   *
   * A synchronous body that throws disposes what it collected and rethrows.
   * An asynchronous body is never cancelled: disposal waits for it to finish
   * or reach `scope.checkpoint()`. Its failure disposes what it collected, is
   * logged, and rejects `handle.ready()`.
   */
  effect(body: (scope: EffectScope) => EffectResult, label = "anonymous"): EffectHandle {
    const root = this._root;
    root.with((st) => st.assertActive(this._fiber));
    const cell = new EffectCell(label, this._fiber);
    const onError = (error: unknown) => root.logInternal(this._fiber, error);
    let result: EffectResult;
    try {
      result = body(new EffectScope(root, cell));
    } catch (error) {
      runSequence(cell.takeDisposers(true))?.catch(onError);
      throw error;
    }
    if (isPromiseLike(result)) {
      cell.setup = Promise.resolve(result).then(
        (disposer) => {
          if (typeof disposer === "function") cell.pushDisposer(disposer, onError);
          return undefined;
        },
        async (error: unknown) => {
          if (error instanceof Interrupted) return undefined;
          await runSequence(cell.takeDisposers(false))?.catch(onError);
          onError(error);
          return error ?? new Error(String(error));
        },
      );
    } else if (typeof result === "function") {
      cell.pushDisposer(result, onError);
    }
    root.run((st) => st.attachEffect(this._fiber, cell));
    return new EffectHandle(root, cell);
  }

  /** Registers an inverse on this context's fiber. */
  onDispose(disposer: Disposer, label = "ctx.onDispose()"): EffectHandle {
    return this.effect((scope) => scope.defer(disposer), label);
  }

  /**
   * Offers configuration updates of this fiber to `accept` before restarting
   * it: returning `true` takes the update over, and the fiber keeps running.
   * `accept` sees the decoded configuration.
   */
  onUpdate(accept: (config: any) => boolean): EffectHandle {
    const root = this._root;
    return this.effect((scope) => {
      const hooks = root.state.fiber(this._fiber)!.updateHooks;
      const id = root.state.next("hook");
      const hook = { removed: false, accept };
      hooks.set(id, hook);
      scope.defer(() => {
        hook.removed = true;
        hooks.delete(id);
      });
    }, "ctx.onUpdate()");
  }

  // Events

  /** Registers a listener as an effect of this context's fiber. */
  on<K extends EventName>(name: K, listener: Listener<K>, options: ListenOptions = {}): EffectHandle {
    return listen(this, name, listener as (...args: any[]) => unknown, !!options.prepend);
  }

  /** Registers a listener that removes itself after its first call. */
  once<K extends EventName>(name: K, listener: Listener<K>, options: ListenOptions = {}): EffectHandle {
    const handle: EffectHandle = this.on(
      name,
      ((...args: any[]) => {
        void handle.dispose();
        return (listener as (...args: any[]) => unknown)(...args);
      }) as Listener<K>,
      options,
    );
    return handle;
  }

  /** Calls every listener. A failing listener is logged and the rest still run; asynchronous ones are not awaited. */
  emit<K extends EventName>(name: K, ...args: EventArgs<K>): void {
    dispatch(this._root, name, args);
  }

  /** Calls listeners until one returns something other than `undefined`, `null` or `false`, and returns it. */
  bail<K extends EventName>(name: K, ...args: EventArgs<K>): ReturnType<Listener<K>> | undefined {
    return bail(this._root, name, args) as ReturnType<Listener<K>> | undefined;
  }

  /** Awaits listeners one at a time until one bails, and returns its result. */
  serial<K extends EventName>(name: K, ...args: EventArgs<K>): Promise<Awaited<ReturnType<Listener<K>>> | undefined> {
    return serial(this._root, name, args) as Promise<Awaited<ReturnType<Listener<K>>> | undefined>;
  }

  /** Awaits every listener concurrently; rejects with an `AggregateError` if any failed. */
  parallel<K extends EventName>(name: K, ...args: EventArgs<K>): Promise<void> {
    return parallel(this._root, name, args);
  }

  // Logging

  /** The logger of this context: named by `intercept("logger", { name })`, by `name`, or after the fiber's plugin. */
  logger(name?: string): Logger {
    const meta = this.interceptMeta<LoggerIntercept>("logger");
    const fallback = () => hyphenate(this._root.state.fiberName(this._fiber));
    return new Logger(this._root, name ?? meta.name ?? fallback(), this._fiber, meta.level);
  }

  /** Registers a log exporter as an effect of this context's fiber. */
  addExporter(exporter: Exporter): EffectHandle {
    const state = this._root.logger;
    return this.effect((scope) => {
      const id = ++state.nextExporter;
      state.exporters.set(id, exporter);
      scope.defer(() => {
        state.exporters.delete(id);
      });
    }, "ctx.logger.exporter()");
  }

  /** The most recent messages, oldest first. */
  logBuffer(): Message[] {
    return [...this._root.logger.buffer];
  }

  setLogBufferSize(size: number) {
    const state = this._root.logger;
    state.bufferSize = size;
    if (state.buffer.length > size) state.buffer.splice(0, state.buffer.length - size);
  }
}
