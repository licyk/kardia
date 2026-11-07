import type { BridgeConfig, ClientRequest, HostMessage, OpenReply, OpenRequest, Transport } from "@kardiajs/bridge";
import { Channel, invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";

export interface TauriTransportOptions {
  /** The Tauri command that hands requests to `kardia_bridge::Bridge::handle` (default `kardia_bridge`). */
  command?: string;
}

/**
 * A bridge transport over one Tauri command: every request is
 * `invoke(command, { request })`, and `open` also passes a `Channel` that
 * carries the host's messages for the session.
 */
export function tauriTransport(options: TauriTransportOptions = {}): Transport {
  const command = options.command ?? "kardia_bridge";
  return {
    open(request: OpenRequest, onMessage: (message: HostMessage) => void) {
      // the setter rather than the constructor argument, which @tauri-apps/api 2.0 lacks
      const channel = new Channel<HostMessage>();
      channel.onmessage = onMessage;
      return invoke<OpenReply>(command, { request, channel });
    },
    request(request: ClientRequest) {
      return invoke(command, { request });
    },
  };
}

/**
 * The configuration of a `Bridge` plugin in a Tauri webview: the Tauri
 * transport, with the webview's label as the client label, so a reloaded
 * page replaces its own earlier session and each webview has its own.
 *
 * ```ts
 * root.plugin(Bridge, tauriBridge());
 * ```
 */
export function tauriBridge(options: TauriTransportOptions & Omit<BridgeConfig, "transport"> = {}): BridgeConfig {
  const { command, ...config } = options;
  return { client: getCurrentWebview().label, ...config, transport: tauriTransport({ command }) };
}
