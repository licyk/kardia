import type { Context } from "@kardiajs/core";

/** In-flight callbacks belong to the scheduling plugin, including after its timer has fired. */
export class Callbacks {
  private stopped = false;
  private readonly pending = new Set<Promise<void>>();

  constructor(private readonly caller: Context) {}

  invoke(callback: () => unknown): Promise<void> {
    if (this.stopped) return Promise.resolve();
    let result: unknown;
    try {
      result = callback();
    } catch (error) {
      this.caller.logger().error(error);
      return Promise.resolve();
    }
    const done = Promise.resolve(result).then(
      () => {},
      (error) => this.caller.logger().error(error),
    );
    this.pending.add(done);
    void done.then(() => this.pending.delete(done));
    return done;
  }

  async stop(): Promise<void> {
    this.stopped = true;
    await Promise.all(this.pending);
  }
}
