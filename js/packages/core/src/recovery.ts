import { ValidationError } from "#/errors";
import type { Fiber } from "#/fiber";

export type RecoveryAction = { type: "keep-failed" } | { type: "retry"; delay: number };

export interface Failure {
  fiber: Fiber;
  error: Error;
  /** Aborted when this decision is superseded or the plugin is removed. */
  signal: AbortSignal;
}

/** Isolation is mandatory under every policy. Manual is the default. Delays are milliseconds. */
export type RecoveryPolicy =
  | { type: "manual" }
  | { type: "retry"; delay: number }
  | { type: "host"; handler: (failure: Failure) => RecoveryAction | PromiseLike<RecoveryAction> };

/** @internal Lives on the fiber, outside any one instance's effects. */
export class Recovery {
  private pending: AbortController | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private observed: { error: Error; revision: number } | undefined;

  constructor(private readonly fiber: Fiber, private readonly policy: Exclude<RecoveryPolicy, { type: "manual" }>) {}

  cancel() {
    clearTimeout(this.timer);
    this.timer = undefined;
    const pending = this.pending;
    this.pending = undefined;
    pending?.abort();
  }

  check() {
    const data = this.fiber.ctx._root.state.fiber(this.fiber.id);
    if (!data || data.removed || data.state !== "failed" || data.inertia || !data.error) {
      this.cancel();
      return;
    }
    const { error, revision } = data;
    if (this.observed?.error === error && this.observed.revision === revision) return;
    this.cancel();
    this.observed = { error, revision };
    const pending = this.pending = new AbortController();
    void this.decide(error, revision, pending).catch((reason) => this.fiber.ctx.logger().error(reason));
  }

  private async decide(error: Error, revision: number, pending: AbortController) {
    const action = this.policy.type === "retry"
      ? { type: "retry" as const, delay: this.policy.delay }
      : await this.policy.handler({ fiber: this.fiber, error, signal: pending.signal });
    if (!this.current(error, revision, pending) || action.type === "keep-failed" || error instanceof ValidationError) return;
    validateDelay(action.delay);
    this.timer = setTimeout(() => {
      this.timer = undefined;
      if (!this.current(error, revision, pending)) return;
      // update clears the failure without writing the unchanged config back to the host.
      void this.fiber.update(this.fiber.config, true).catch(() => {});
    }, action.delay);
  }

  private current(error: Error, revision: number, pending: AbortController) {
    const data = this.fiber.ctx._root.state.fiber(this.fiber.id);
    return !pending.signal.aborted && this.pending === pending && data?.recovery === this &&
      !data.removed && data.state === "failed" && data.error === error && data.revision === revision;
  }
}

/** JS timers overflow above this bound and would turn a long delay into a tight retry loop. */
export function validateDelay(delay: number) {
  if (!Number.isFinite(delay) || delay <= 0 || delay > 2_147_483_647) {
    throw new RangeError("recovery delay must be greater than zero and at most 2147483647 milliseconds");
  }
}
