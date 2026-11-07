/** The protocol version, matching `kardia_bridge::PROTOCOL`. */
export const PROTOCOL = 1;

export type BridgeErrorType = "unavailable" | "timeout" | "invalid" | "failed";

/** A failed bridge request: the host's `BridgeError`, or one raised on this side. */
export class BridgeError extends Error {
  override name = "BridgeError";

  constructor(
    readonly type: BridgeErrorType,
    message: string,
  ) {
    super(message);
  }

  /**
   * Normalizes anything a transport or a handler rejects with: a
   * `{ type, message }` object from the host keeps its type; anything else
   * (a transport's own error, a thrown value) becomes `failed`.
   */
  static from(reason: unknown): BridgeError {
    if (reason instanceof BridgeError) return reason;
    if (isWireError(reason)) return new BridgeError(reason.type, reason.message);
    if (reason instanceof Error) return new BridgeError("failed", reason.message);
    return new BridgeError("failed", typeof reason === "string" ? reason : JSON.stringify(reason) ?? String(reason));
  }

  /** The wire form, as sent in a `reject`. */
  toJSON(): WireError {
    return { type: this.type, message: this.message };
  }
}

export interface WireError {
  type: BridgeErrorType;
  message: string;
}

const ERROR_TYPES: readonly string[] = ["unavailable", "timeout", "invalid", "failed"];

function isWireError(value: unknown): value is WireError {
  return (
    typeof value === "object" &&
    value !== null &&
    ERROR_TYPES.includes((value as WireError).type) &&
    typeof (value as WireError).message === "string"
  );
}

/** A service the host exposes. A new `id` is a new exposure of the same name. */
export interface ServiceInfo {
  name: string;
  id: number;
  value: unknown;
}

export interface OpenRequest {
  type: "open";
  protocol: number;
  client: string;
}

export interface OpenReply {
  session: number;
  protocol: number;
  services: ServiceInfo[];
}

export type ClientRequest =
  | { type: "close"; session: number }
  | { type: "call"; session: number; service: string; method: string; args: unknown }
  | { type: "resolve"; session: number; id: number; value: unknown }
  | { type: "reject"; session: number; id: number; error: WireError }
  | { type: "provide"; session: number; service: string; value: unknown }
  | { type: "withdraw"; session: number; service: string };

/** Why the host ended a session: the bridge stopped, or a newer session of the same client replaced it. */
export type CloseReason = "stopped" | "replaced";

export type HostMessage =
  | { type: "services"; services: ServiceInfo[] }
  | { type: "event"; name: string; payload: unknown }
  | { type: "request"; id: number; service: string; method: string; args: unknown }
  | { type: "closed"; reason: CloseReason };

/**
 * How the client reaches the host: a request/response channel, plus a
 * message stream opened with the session. `@kardiajs/tauri` implements it
 * with one Tauri command and a `Channel`.
 */
export interface Transport {
  /** Opens a session; `onMessage` receives what the host pushes to it from then on. */
  open(request: OpenRequest, onMessage: (message: HostMessage) => void): Promise<OpenReply>;
  /** Sends a request and resolves with its result, or rejects (ideally with a `{ type, message }` error). */
  request(request: ClientRequest): Promise<unknown>;
}
