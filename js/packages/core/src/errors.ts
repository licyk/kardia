/** Errors raised by the runtime itself. Messages match Kardia's. */
export class KardiaError extends Error {
  override name = "KardiaError";

  constructor(
    readonly code: "inactive-effect" | "already-provided",
    message: string,
  ) {
    super(message);
  }

  static inactiveEffect() {
    return new KardiaError("inactive-effect", "cannot create effect on inactive context");
  }

  static alreadyProvided(key: string, by: string) {
    return new KardiaError("already-provided", `service "${key}" has been registered at <${by}>`);
  }
}

/** A checked service access that cannot be served. */
export class AccessError extends Error {
  override name = "AccessError";

  constructor(
    readonly code: "undeclared" | "inactive",
    readonly key: string,
  ) {
    super(
      code === "undeclared"
        ? `cannot get property "${key}" without inject`
        : `cannot get required service "${key}" in inactive context`,
    );
  }
}

/**
 * Thrown by an iteration boundary ({@link EffectScope.checkpoint}) once its
 * effect has been disposed. Ending an effect body with it is not a failure.
 */
export class Interrupted extends Error {
  override name = "Interrupted";

  constructor() {
    super("interrupted at an iteration boundary");
  }
}

/** A path segment of a configuration issue. */
export type PathSegment = string | number | symbol;

export interface Issue {
  message: string;
  path?: readonly PathSegment[];
}

/** A configuration rejected by a plugin's `Config` schema. */
export class ValidationError extends TypeError {
  override name = "ValidationError";

  constructor(readonly issues: readonly Issue[]) {
    super(
      "invalid config:\n" +
        issues
          .map((issue) =>
            issue.path?.length ? `  - ${issue.message} (at ${issue.path.map(String).join(".")})` : `  - ${issue.message}`,
          )
          .join("\n"),
    );
  }
}

/** Normalizes a thrown value into an `Error`. */
export function toError(reason: unknown): Error {
  return reason instanceof Error ? reason : new Error(String(reason));
}
