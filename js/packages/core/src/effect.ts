import { Interrupted } from "#/errors";
import type { Root } from "#/root";
import type { FiberId } from "#/scope";

/** The inverse of an effect. It may finish asynchronously. */
export type Disposer = () => void | PromiseLike<void>;

/** Introspection of an effect: its label and the effects it adopted. */
export interface EffectMeta {
  label: string;
  children: EffectMeta[];
}

export function isPromiseLike(value: unknown): value is PromiseLike<unknown> {
  return (
    (typeof value === "object" || typeof value === "function") &&
    value !== null &&
    typeof (value as PromiseLike<unknown>).then === "function"
  );
}

/**
 * Runs disposers newest first, one after another. Synchronous disposers run
 * before this returns; from the first asynchronous one on, the rest waits for
 * it. Every disposer runs even if an earlier one fails; the first error wins.
 * Returns `undefined` when everything finished synchronously without error.
 */
export function runSequence(disposers: Disposer[]): Promise<void> | undefined {
  const errors: unknown[] = [];
  while (disposers.length) {
    const disposer = disposers.pop()!;
    let result: unknown;
    try {
      result = disposer();
    } catch (error) {
      errors.push(error);
      continue;
    }
    if (isPromiseLike(result)) return finishSequence(result, disposers, errors);
  }
  return errors.length ? Promise.reject(errors[0]) : undefined;
}

async function finishSequence(pending: PromiseLike<unknown>, rest: Disposer[], errors: unknown[]) {
  try {
    await pending;
  } catch (error) {
    errors.push(error);
  }
  while (rest.length) {
    try {
      await rest.pop()!();
    } catch (error) {
      errors.push(error);
    }
  }
  if (errors.length) throw errors[0];
}

/** One effect: its collected inverses, adopted effects and, for an asynchronous body, its setup. */
export class EffectCell {
  active = true;
  /** Disposers were taken for the final run; late ones run immediately. */
  private closed = false;
  /** Slot in the owning fiber's accumulator, while registered there. */
  entry: number | undefined;
  private disposers: Disposer[] = [];
  private children: EffectCell[] = [];
  /** The asynchronous body: resolves with its failure, never rejects. */
  setup: Promise<unknown> | undefined;
  private disposal: Promise<void> | undefined;
  private disposed = false;

  constructor(
    readonly label: string,
    readonly fiber: FiberId,
  ) {}

  pushDisposer(disposer: Disposer, onError: (error: unknown) => void) {
    if (this.closed) {
      runSequence([disposer])?.catch(onError);
    } else {
      this.disposers.push(disposer);
    }
  }

  adopt(child: EffectCell) {
    this.children.push(child);
  }

  takeDisposers(close: boolean) {
    this.closed ||= close;
    return this.disposers.splice(0);
  }

  meta(): EffectMeta {
    return { label: this.label, children: this.children.map((child) => child.meta()) };
  }

  /**
   * Starts disposal at most once; waits for an in-flight setup first. Returns
   * `undefined` when it finished synchronously without error.
   */
  dispose(root: Root | undefined): Promise<void> | undefined {
    if (this.disposed) return this.disposal;
    this.disposed = true;
    this.active = false;
    const entry = this.entry;
    this.entry = undefined;
    if (entry !== undefined && root) root.run((st) => st.detachEffect(this.fiber, entry));
    this.disposal = this.setup
      ? this.setup.then(() => runSequence(this.takeDisposers(true)))
      : runSequence(this.takeDisposers(true));
    return this.disposal;
  }
}

/** Disposals whose failure is already being logged. */
const reported = new WeakSet<Promise<void>>();

/**
 * A non-owning handle to an effect. Dropping it does not dispose the effect:
 * its lifetime is governed by the fiber it was registered on.
 */
export class EffectHandle {
  /** @internal */
  constructor(
    private readonly root: Root,
    /** @internal */ readonly cell: EffectCell,
  ) {}

  get label() {
    return this.cell.label;
  }

  get active() {
    return this.cell.active;
  }

  /**
   * Reverts the effect (at most once). Inverses run newest first; the
   * synchronous ones have run when this returns. A failing inverse is logged,
   * and the returned promise rejects with the first failure (it is marked
   * handled, so dropping it is safe).
   */
  dispose(): Promise<void> {
    const disposal = this.cell.dispose(this.root);
    if (!disposal) return Promise.resolve();
    if (!reported.has(disposal)) {
      reported.add(disposal);
      disposal.catch((error) => this.root.logInternal(this.cell.fiber, error));
    }
    return disposal;
  }

  /** Resolves when the setup has finished, rejecting with its failure. */
  async ready(): Promise<void> {
    const failure = await this.cell.setup;
    if (failure !== undefined) throw failure;
  }

  meta(): EffectMeta {
    return this.cell.meta();
  }
}

/**
 * The collector handed to an effect body. Inverses collected here are
 * disposed together, newest first.
 */
export class EffectScope {
  /** @internal */
  constructor(
    private readonly root: Root,
    private readonly cell: EffectCell,
  ) {}

  /** Whether the effect is still in place. */
  get active() {
    return this.cell.active;
  }

  /** Collects an inverse. */
  defer(disposer: Disposer) {
    this.cell.pushDisposer(disposer, (error) => this.root.logInternal(this.cell.fiber, error));
  }

  /**
   * Moves a nested effect under this one: it is no longer disposed by its
   * fiber directly, but together with this effect.
   */
  adopt(handle: EffectHandle) {
    const child = handle.cell;
    const entry = child.entry;
    if (entry !== undefined) {
      child.entry = undefined;
      this.root.run((st) => st.detachEffect(child.fiber, entry));
    }
    this.cell.adopt(child);
    this.defer(() => child.dispose(this.root));
  }

  /** An iteration boundary: throws {@link Interrupted} once the effect has been disposed. */
  checkpoint() {
    if (!this.cell.active) throw new Interrupted();
  }
}
