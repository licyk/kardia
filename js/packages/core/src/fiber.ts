import { Context } from "#/context";
import type { EffectMeta } from "#/effect";
import { refresh } from "#/lifecycle";
import type { Plugin } from "#/plugin";
import type { Root } from "#/root";
import { type FiberId, ROOT_FIBER, type Scope } from "#/scope";
import type { FiberData, FiberState } from "#/state";

/**
 * Marks a promise as handled while still returning it: the fiber reports its
 * own failures, so a caller that drops the promise must not cause an
 * unhandled rejection, while `await` still observes the failure.
 */
function handled<T>(promise: Promise<T>): Promise<T> {
  promise.catch(() => {});
  return promise;
}

/** `name` stands in once the fiber has been collected, for events dispatched after that. */
export function fiberHandle(root: Root, id: FiberId, scope: Scope, name?: string) {
  return new Fiber(new Context(root, id, scope), name);
}

/** A handle to a fiber: one instantiation of a plugin. */
export class Fiber {
  /** @internal */
  constructor(
    readonly ctx: Context,
    private readonly lastName?: string,
  ) {}

  private get root() {
    return this.ctx._root;
  }

  private read<T>(f: (fiber: FiberData) => T): T | undefined {
    const fiber = this.root.state.fiber(this.id);
    return fiber && f(fiber);
  }

  get id(): FiberId {
    return this.ctx._fiber;
  }

  get isRoot() {
    return this.id === ROOT_FIBER;
  }

  /** The fiber's state; `disposed` once it has been collected. */
  get state(): FiberState {
    return this.read((fiber) => fiber.state) ?? "disposed";
  }

  /** Whether the fiber has been retired (it may still be unloading). */
  get removed() {
    return this.read((fiber) => fiber.removed) ?? true;
  }

  /** The source configuration, as given. */
  get config(): unknown {
    return this.read((fiber) => fiber.config);
  }

  get inject(): ReadonlyMap<string, unknown> {
    return this.read((fiber) => fiber.inject) ?? new Map();
  }

  /** The failure of the last load, if the fiber is `failed`. */
  get error(): Error | undefined {
    return this.read((fiber) => fiber.error);
  }

  /** The plugin's name, or its nearest named ancestor's. */
  get name(): string {
    if (this.lastName !== undefined && !this.root.state.fiber(this.id)) return this.lastName;
    return this.root.state.fiberName(this.id);
  }

  /** The plugin the fiber runs (after replacements). */
  get plugin(): Plugin | undefined {
    return this.read((fiber) => fiber.runtime) as Plugin | undefined;
  }

  get parent(): Fiber | undefined {
    if (this.isRoot) return;
    const parent = this.read((fiber) => fiber.parent);
    const scope = parent === undefined ? undefined : this.root.state.fiber(parent)?.scope;
    return scope && fiberHandle(this.root, parent!, scope);
  }

  /** The effects registered on the fiber, oldest first. */
  effects(): EffectMeta[] {
    return this.read((fiber) => [...fiber.disposables.values()].map((cell) => cell.meta())) ?? [];
  }

  /** Waits until no transition is in flight; rejects with the fiber's failure. */
  async settled(): Promise<void> {
    await this.root.waitUntil((st) => !st.fiber(this.id)?.inertia);
    const error = this.error;
    if (error) throw error;
  }

  /** Retires the fiber. For the root fiber this restarts it, reverting everything registered on it. */
  dispose(): Promise<void> {
    if (this.isRoot) return this.restart().catch(() => {});
    const cell = this.read((fiber) => fiber.retire);
    return cell?.dispose(this.root) ?? Promise.resolve();
  }

  /**
   * Unloads and loads the fiber again; resolves once it settles. A failed
   * fiber stays failed: only `update` and `replace` clear its error. Throws
   * for a retired fiber.
   */
  restart(): Promise<void> {
    this.root.run((st) => {
      st.assertActive(this.id);
      st.fiber(this.id)!.revision++;
      refresh(st, this.id);
    });
    return handled(this.settled());
  }

  /**
   * Replaces the configuration. A loaded fiber validates it first: a rejected
   * config throws a `ValidationError` and leaves the running instance
   * untouched. The config is then offered to the fiber's update hooks
   * (`ctx.onUpdate`); unless one takes it over, the fiber restarts. Resolves
   * once the fiber settles, rejecting with a failed reload. `noSave`
   * suppresses `internal/commit`.
   */
  update(config: unknown, noSave = false): Promise<void> {
    let resolved: { config: unknown } | undefined;
    const { loaded, def, hooks } = this.root.with((st) => {
      st.assertActive(this.id);
      const fiber = st.fiber(this.id)!;
      return { loaded: fiber.target !== null, def: st.defOf(this.id), hooks: fiber.updateHooks };
    });
    if (loaded && def) resolved = { config: def.decode(config) };
    if (!noSave) this.ctx.root.emit("internal/commit", this, config);
    this.root.run((st) => {
      const fiber = st.fiber(this.id);
      if (!fiber) return;
      fiber.config = config;
      fiber.error = undefined;
    });
    if (resolved) {
      for (const hook of [...hooks.values()]) {
        if (!hook.removed && hook.accept(resolved.config)) return Promise.resolve();
      }
    }
    return this.restart();
  }
}
