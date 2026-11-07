import { vi } from "vitest";

import { Context, type Message } from "#/index";

export function sleep(ms = 0) {
  return new Promise<void>((resolve) => setTimeout(resolve, ms));
}

/** Runs `fn` with fake timers and a fresh root. */
export function withTimers(fn: (root: Context) => Promise<void>) {
  return async () => {
    vi.useFakeTimers();
    try {
      await fn(new Context());
    } finally {
      vi.useRealTimers();
    }
  };
}

/**
 * Collects every message logged under `root` (at every level). The exporter
 * is not an effect, so it outlives a restart of the root fiber.
 */
export function captureLogs(root: Context) {
  const messages: Message[] = [];
  root._root.logger.exporters.set(-1, { export: (message) => messages.push(message), level: () => 3 });
  return {
    messages,
    errors: () => messages.filter((message) => message.level === 0),
    warnings: () => messages.filter((message) => message.level === 1),
  };
}

export function listenerCount(root: Context, name: string) {
  return root._root.state.listeners.get(name)?.length ?? 0;
}
