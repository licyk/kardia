import { Bridge, type ClientRequest, type HostMessage, type RemoteService } from "@kardiajs/bridge";
import { Context } from "@kardiajs/core";
import type { Channel } from "@tauri-apps/api/core";
import { clearMocks, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import { tauriBridge } from "../src/index.ts";

// @tauri-apps/api reaches its IPC through `window.__TAURI_INTERNALS__`, which its mocks fill in.
beforeAll(() => {
  (globalThis as { window?: unknown }).window ??= globalThis;
});

afterEach(() => clearMocks());

async function until(predicate: () => boolean) {
  for (let i = 0; i < 100 && !predicate(); i++) await new Promise((resolve) => setTimeout(resolve, 5));
  expect(predicate()).toBe(true);
}

/** Pushes a message on a channel, as the Rust side of `tauri::ipc::Channel` does. */
function push(channel: Channel<HostMessage>, index: number, message: HostMessage) {
  (window as any).__TAURI_INTERNALS__.runCallback(channel.id, { index, message });
}

describe("Tauri transport", () => {
  it("invokes one command, labels the client after the webview and streams over a Channel", async () => {
    mockWindows("settings");
    const invocations: { cmd: string; request: ClientRequest | { type: "open" } }[] = [];
    let channel: Channel<HostMessage> | undefined;
    mockIPC((cmd, args: any) => {
      invocations.push({ cmd, request: args.request });
      if (args.request.type === "open") {
        channel = args.channel;
        return { session: 1, protocol: 1, services: [{ name: "echo", id: 1, value: null }] };
      }
      if (args.request.type === "call") return Promise.reject({ type: "unavailable", message: "gone" });
      return null;
    });

    const root = new Context();
    root.plugin(Bridge, tauriBridge({ command: "my_bridge" }));
    await until(() => !!root.live("host.echo"));
    expect(invocations[0]).toEqual({ cmd: "my_bridge", request: { type: "open", protocol: 1, client: "settings" } });

    push(channel!, 0, { type: "services", services: [{ name: "status", id: 2, value: { ok: true } }] });
    await until(() => !!root.live("host.status"));
    expect(root.live("host.echo")).toBeUndefined();
    expect((root.live("host.status") as RemoteService).value).toEqual({ ok: true });

    await expect(root.get("bridge").call("status", "get")).rejects.toMatchObject({ type: "unavailable", message: "gone" });
    expect(invocations.at(-1)).toEqual({
      cmd: "my_bridge",
      request: { type: "call", session: 1, service: "status", method: "get", args: null },
    });
  });
});
