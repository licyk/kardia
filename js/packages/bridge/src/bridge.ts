import { type Context, type EffectHandle, type Logger, Service } from "@kardiajs/core";

import {
  BridgeError,
  type ClientRequest,
  type HostMessage,
  type OpenReply,
  PROTOCOL,
  type ServiceInfo,
  type Transport,
} from "#/protocol";

declare module "@kardiajs/core" {
  interface Services {
    bridge: Bridge;
  }

  interface Events {
    /** The bridge connected, lost its session, or was replaced by another client with the same label. */
    "bridge/status"(status: BridgeStatus): void;
  }
}

export type BridgeStatus = "connecting" | "connected" | "disconnected" | "replaced";

export interface BridgeConfig {
  transport: Transport;
  /** This client's label. A new session replaces the host's older sessions with the same label (default `main`). */
  client?: string;
  /** Host services appear as `<prefix>.<name>`, host events as `<prefix>/<name>` (default `host`). */
  prefix?: string;
  /** Milliseconds a request may take before it fails with `timeout` (default 30000). The host keeps running it. */
  timeout?: number;
  /** Milliseconds between attempts to (re)connect (default 1000). */
  retryDelay?: number;
}

/** Method signatures of a host service, for typed calls. */
export type RemoteMethods = Record<string, (args?: any) => unknown>;

/**
 * A service the host exposes, as this client sees it: the exposure's JSON
 * value, and its methods, called across the bridge. It is provided as
 * `<prefix>.<name>` exactly while the host exposes it, so plugins injecting
 * it run while the host service is there, and restart when it is exposed anew.
 */
export class RemoteService<M extends RemoteMethods = RemoteMethods> {
  /** @internal */
  constructor(
    private readonly bridge: Bridge,
    readonly name: string,
    readonly id: number,
    readonly value: unknown,
  ) {}

  /** Calls a method of the host service. Rejects with a {@link BridgeError}. */
  call<K extends keyof M & string>(method: K, args?: Parameters<M[K]>[0]): Promise<Awaited<ReturnType<M[K]>>> {
    return this.bridge.call(this.name, method, args) as Promise<Awaited<ReturnType<M[K]>>>;
  }
}

/** A service this client provides to the host: a JSON value, and methods the host may call. */
export interface LocalService {
  value?: unknown;
  methods?: Record<string, (args: any) => unknown>;
}

/**
 * The client side of the kardia bridge, and the `bridge` service. Mount it
 * as a plugin: `ctx.plugin(Bridge, { transport })`.
 *
 * It connects in the background and reconnects when the host's bridge
 * restarts, so the plugin is active while disconnected; host services are
 * simply absent until then.
 */
export class Bridge extends Service {
  static readonly provide = "bridge";

  private readonly options: Required<BridgeConfig>;
  private session: number | undefined;
  /** Identifies the current connection attempt; messages and replies of older ones are ignored. */
  private attempt = 0;
  private stopped = false;
  private retry: ReturnType<typeof setTimeout> | undefined;
  private readonly mirrored = new Map<string, { id: number; handle: EffectHandle }>();
  private readonly local = new Map<string, LocalService>();
  private currentStatus: BridgeStatus = "connecting";

  constructor(ctx: Context, config: BridgeConfig) {
    super(ctx, "bridge");
    this.options = { client: "main", prefix: "host", timeout: 30_000, retryDelay: 1_000, ...config };
    ctx.onDispose(() => this.stop());
    this.connect();
  }

  get status(): BridgeStatus {
    return this.currentStatus;
  }

  private get log(): Logger {
    return this.ctx.logger();
  }

  /** Calls a method of a host service. Rejects with a {@link BridgeError}; `unavailable` while disconnected. */
  call(service: string, method: string, args?: unknown): Promise<unknown> {
    const session = this.session;
    if (session === undefined) return Promise.reject(new BridgeError("unavailable", "the bridge is not connected"));
    return this.send({ type: "call", session, service, method, args: args ?? null });
  }

  /**
   * Provides a service to the host as `name`, as an effect of the calling
   * plugin: the host sees it while that plugin runs and this client is
   * connected (it is provided again after every reconnect).
   */
  provide(name: string, service: LocalService = {}): EffectHandle {
    return this.caller.effect((scope) => {
      if (this.local.has(name)) throw new BridgeError("invalid", `service ${name} is already provided by this client`);
      this.local.set(name, service);
      this.sendProvide(name, service);
      scope.defer(() => {
        if (this.local.get(name) !== service) return;
        this.local.delete(name);
        const session = this.session;
        if (session !== undefined) this.send({ type: "withdraw", session, service: name }).catch(() => {});
      });
    }, `bridge.provide(${JSON.stringify(name)})`);
  }

  private connect() {
    if (this.stopped) return;
    const attempt = ++this.attempt;
    const queue: HostMessage[] = [];
    let ready = false;
    this.setStatus("connecting");
    const onMessage = (message: HostMessage) => {
      if (attempt !== this.attempt) return;
      // apply the reply's snapshot before anything pushed after it
      if (ready) this.receive(message);
      else queue.push(message);
    };
    const request = { type: "open" as const, protocol: PROTOCOL, client: this.options.client };
    withTimeout(this.options.transport.open(request, onMessage), this.options.timeout, "open").then(
      (reply) => {
        if (attempt !== this.attempt) {
          this.options.transport.request({ type: "close", session: reply.session }).catch(() => {});
          return;
        }
        this.opened(reply);
        ready = true;
        for (const message of queue.splice(0)) {
          if (attempt === this.attempt) this.receive(message);
        }
      },
      (reason) => {
        if (attempt !== this.attempt) return;
        this.log.debug(`cannot connect: ${BridgeError.from(reason).message}`);
        this.setStatus("disconnected");
        this.scheduleReconnect();
      },
    );
  }

  private opened(reply: OpenReply) {
    this.session = reply.session;
    this.reconcile(reply.services);
    for (const [name, service] of this.local) this.sendProvide(name, service);
    this.setStatus("connected");
  }

  private receive(message: HostMessage) {
    // called from the transport's callbacks, where nothing would catch a failure
    try {
      this.dispatch(message);
    } catch (error) {
      this.log.error(error);
    }
  }

  private dispatch(message: HostMessage) {
    switch (message.type) {
      case "services":
        this.reconcile(message.services);
        break;
      case "event":
        this.ctx.emit(`${this.options.prefix}/${message.name}`, message.payload);
        break;
      case "request":
        void this.answer(message);
        break;
      case "closed":
        this.disconnect();
        if (message.reason === "replaced") {
          this.log.warn(`another client labelled "${this.options.client}" replaced this session`);
          this.setStatus("replaced");
        } else {
          this.scheduleReconnect();
        }
        break;
    }
  }

  /** Makes the mirrored services match the host's: a new `id` is a new exposure, so its dependents restart. */
  private reconcile(services: ServiceInfo[]) {
    if (this.stopped) return;
    const next = new Map(services.map((info) => [info.name, info]));
    for (const [name, entry] of this.mirrored) {
      if (next.get(name)?.id === entry.id) continue;
      // the realm is free once the synchronous part of the withdrawal has run
      void entry.handle.dispose();
      this.mirrored.delete(name);
    }
    for (const info of services) {
      if (this.mirrored.has(info.name)) continue;
      const remote = new RemoteService(this, info.name, info.id, info.value);
      const handle = this.ctx.provide(`${this.options.prefix}.${info.name}`, remote);
      this.mirrored.set(info.name, { id: info.id, handle });
    }
  }

  private async answer(message: Extract<HostMessage, { type: "request" }>) {
    const session = this.session;
    let reply: ClientRequest;
    try {
      const service = this.local.get(message.service);
      if (!service) throw new BridgeError("unavailable", `service ${message.service} is not provided by this client`);
      const method = service.methods?.[message.method];
      if (!method) throw new BridgeError("invalid", `unknown method ${message.service}.${message.method}`);
      const value = await method(message.args);
      reply = { type: "resolve", session: session!, id: message.id, value: value ?? null };
    } catch (error) {
      reply = { type: "reject", session: session!, id: message.id, error: BridgeError.from(error).toJSON() };
    }
    if (session === undefined || session !== this.session) return;
    await this.send(reply).catch((error) => this.log.warn(`cannot answer ${message.service}.${message.method}:`, error));
  }

  private sendProvide(name: string, service: LocalService) {
    const session = this.session;
    if (session === undefined) return;
    this.send({ type: "provide", session, service: name, value: service.value ?? null }).catch((error) =>
      this.log.warn(`cannot provide ${name} to the host:`, error),
    );
  }

  private send(request: ClientRequest): Promise<unknown> {
    return withTimeout(this.options.transport.request(request), this.options.timeout, request.type).catch((reason) => {
      throw BridgeError.from(reason);
    });
  }

  /** The session ended: its host services are gone. */
  private disconnect() {
    this.session = undefined;
    this.attempt++;
    for (const { handle } of this.mirrored.values()) void handle.dispose();
    this.mirrored.clear();
    this.setStatus("disconnected");
  }

  private scheduleReconnect() {
    if (this.stopped) return;
    clearTimeout(this.retry);
    this.retry = setTimeout(() => this.connect(), this.options.retryDelay);
  }

  private stop() {
    this.stopped = true;
    this.attempt++;
    clearTimeout(this.retry);
    const session = this.session;
    this.session = undefined;
    if (session !== undefined) this.options.transport.request({ type: "close", session }).catch(() => {});
    // the mirrored services are effects of this fiber, withdrawn with it
    this.mirrored.clear();
  }

  private setStatus(status: BridgeStatus) {
    if (this.currentStatus === status) return;
    this.currentStatus = status;
    if (!this.stopped) this.ctx.emit("bridge/status", status);
  }
}

function withTimeout<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new BridgeError("timeout", `${what} did not answer within ${ms} ms`)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (reason) => {
        clearTimeout(timer);
        reject(reason);
      },
    );
  });
}
