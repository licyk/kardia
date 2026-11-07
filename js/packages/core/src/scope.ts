/** A realm: the slot a service key resolves to. Each key has a root realm. */
export type Realm = symbol;

export type FiberId = number;

export const ROOT_FIBER: FiberId = 0;

type ScopeKind =
  | { type: "fiber"; fiber: FiberId }
  | { type: "isolate"; key: string; realm: Realm }
  | { type: "intercept"; key: string; meta: unknown };

/** A node of a context's derivation chain: a fiber, an isolation or an interception. */
export class Scope {
  constructor(
    readonly parent: Scope | undefined,
    readonly kind: ScopeKind,
  ) {}

  static root() {
    return new Scope(undefined, { type: "fiber", fiber: ROOT_FIBER });
  }

  child(kind: ScopeKind) {
    return new Scope(this, kind);
  }

  /** This node and its ancestors, nearest first. */
  *ancestors(): Generator<Scope> {
    for (let scope: Scope | undefined = this; scope; scope = scope.parent) yield scope;
  }

  /** This node's own realm override for `key`. */
  isolates(key: string): Realm | undefined {
    return this.kind.type === "isolate" && this.kind.key === key ? this.kind.realm : undefined;
  }

  /** The nearest realm override for `key` on the chain. */
  ownRealm(key: string): Realm | undefined {
    for (const scope of this.ancestors()) {
      const realm = scope.isolates(key);
      if (realm) return realm;
    }
  }

  /** Interception metadata for `key`, from the outermost context to this one. */
  intercepts(key: string): unknown[] {
    const values: unknown[] = [];
    for (const scope of this.ancestors()) {
      if (scope.kind.type === "intercept" && scope.kind.key === key) values.unshift(scope.kind.meta);
    }
    return values;
  }

  /** The nearest fiber node on the chain, starting with this one. */
  fiber(): FiberId {
    for (const scope of this.ancestors()) {
      if (scope.kind.type === "fiber") return scope.kind.fiber;
    }
    return ROOT_FIBER;
  }

  isWithin(ancestor: Scope) {
    for (const scope of this.ancestors()) {
      if (scope === ancestor) return true;
    }
    return false;
  }
}
