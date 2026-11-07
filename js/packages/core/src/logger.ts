import type { Root } from "./root.js";
import type { FiberId } from "./scope.js";

/** Log levels: lower is more severe. A logger exports messages at or below its level. */
export const Level = { error: 0, warn: 1, info: 2, debug: 3 } as const;
export type Level = (typeof Level)[keyof typeof Level];

const LEVEL_NAMES = ["error", "warn", "info", "debug"] as const;

export function levelName(level: Level) {
  return LEVEL_NAMES[level];
}

export interface Message {
  sn: number;
  /** Milliseconds since the Unix epoch. */
  ts: number;
  name: string;
  level: Level;
  text: string;
  fiber: FiberId;
}

/** A log sink. Registering one is an effect of the registering fiber. */
export interface Exporter {
  export(message: Message): void;
  /** The most verbose level exported for logger `name`; `undefined` defers to the logger's own level (default: info). */
  level?(name: string): Level | undefined;
}

/** Interception metadata of the `logger` key: `ctx.intercept("logger", { name, level })`. */
export interface LoggerIntercept {
  name?: string;
  level?: Level;
}

export class LoggerState {
  readonly exporters = new Map<number, Exporter>();
  buffer: Message[] = [];
  bufferSize = 1000;
  sn = 0;
  nextExporter = 0;
}

/** A named logger bound to the fiber it was created for. */
export class Logger {
  /** @internal */
  constructor(
    private readonly root: Root,
    readonly name: string,
    readonly fiber: FiberId,
    readonly level?: Level,
  ) {}

  withLevel(level: Level) {
    return new Logger(this.root, this.name, this.fiber, level);
  }

  error(...args: unknown[]) {
    this.log(Level.error, ...args);
  }

  warn(...args: unknown[]) {
    this.log(Level.warn, ...args);
  }

  info(...args: unknown[]) {
    this.log(Level.info, ...args);
  }

  debug(...args: unknown[]) {
    this.log(Level.debug, ...args);
  }

  /** Logs an error with its stack; an `AggregateError` logs each of its errors. */
  logError(error: unknown, level: Level = Level.error) {
    if (error instanceof AggregateError && error.errors.length) {
      for (const inner of error.errors) this.logError(inner, level);
      return;
    }
    this.log(level, error);
  }

  log(level: Level, ...args: unknown[]) {
    const state = this.root.logger;
    const message: Message = {
      sn: ++state.sn,
      ts: Date.now(),
      name: this.name,
      level,
      text: args.map(format).join(" "),
      fiber: this.fiber,
    };
    if (level <= (this.level ?? Level.info) && state.bufferSize > 0) {
      state.buffer.push(message);
      if (state.buffer.length > state.bufferSize) state.buffer.splice(0, state.buffer.length - state.bufferSize);
    }
    for (const exporter of [...state.exporters.values()]) {
      if (level <= (exporter.level?.(this.name) ?? this.level ?? Level.info)) exporter.export(message);
    }
  }
}

function format(value: unknown): string {
  if (typeof value === "string") return value;
  if (value instanceof Error) return value.stack ?? `${value.name}: ${value.message}`;
  if (typeof value === "object" && value !== null) {
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }
  return String(value);
}

/** `fooBar` → `foo-bar`, `Qux` → `qux`, `snake_case` → `snake-case`. */
export function hyphenate(source: string) {
  let result = "";
  let prevUpper = true;
  for (let index = 0; index < source.length; index++) {
    const ch = source[index];
    if (ch === "_") {
      result += "-";
      prevUpper = false;
    } else if (ch >= "A" && ch <= "Z") {
      if (index > 0 && !prevUpper) result += "-";
      result += ch.toLowerCase();
      prevUpper = true;
    } else {
      result += ch;
      prevUpper = false;
    }
  }
  return result;
}

export interface ConsoleExporterOptions {
  /** The most verbose level per logger name, with `default` for the rest. */
  levels?: Record<string, Level>;
}

/** An exporter that writes to `console`, as `[name] text`. */
export function consoleExporter(options: ConsoleExporterOptions = {}): Exporter {
  const methods = [console.error, console.warn, console.info, console.debug] as const;
  return {
    level: (name) => options.levels?.[name] ?? options.levels?.default,
    export: (message) => methods[message.level].call(console, `[${message.name}] ${message.text}`),
  };
}
