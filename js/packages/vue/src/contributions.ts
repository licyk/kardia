import { type Context, type EffectHandle, type PluginObject, Service } from "@kardiajs/core";
import { type ComputedRef, computed, shallowRef } from "vue";

import { useService } from "#/context";

/** What every contribution may carry: an identity, unique in its registry, and a sort key. */
export interface Contribution {
  id?: string;
  /** Lower comes first; equal orders keep their registration order (default 0). */
  order?: number;
}

/** A contribution, with the context of the plugin that made it. */
export interface ContributionEntry<T> {
  readonly item: T;
  readonly owner: Context;
}

/**
 * A contribution point: a service extensions register items into, such as
 * the views, settings sections or commands of an interface. A registration
 * is an effect of the registering plugin, so its item leaves when that
 * plugin stops. The list is Vue-reactive.
 */
export class Contributions<T extends Contribution = Contribution> extends Service {
  private readonly list = shallowRef<readonly ContributionEntry<T>[]>([]);

  constructor(
    ctx: Context,
    /** The key this registry is provided under. */
    readonly key: string,
  ) {
    super(ctx, key);
  }

  /** The items, ordered. Reading it in a render or `computed` tracks it. */
  get items(): readonly T[] {
    return this.list.value.map((entry) => entry.item);
  }

  /** The items with their owners, ordered. */
  get entries(): readonly ContributionEntry<T>[] {
    return this.list.value;
  }

  /** Adds `item`, as an effect of the calling plugin. Throws if its `id` is taken. */
  register(item: T): EffectHandle {
    const owner = this.caller;
    return owner.effect(() => {
      if (item.id !== undefined && this.list.value.some((entry) => entry.item.id === item.id)) {
        throw new Error(`${this.key} already has a contribution "${item.id}"`);
      }
      const entry: ContributionEntry<T> = { item, owner };
      // Array.prototype.sort is stable: equal orders keep their registration order
      this.list.value = [...this.list.value, entry].sort((a, b) => (a.item.order ?? 0) - (b.item.order ?? 0));
      return () => {
        this.list.value = this.list.value.filter((other) => other !== entry);
      };
    }, `${this.key}.register(${item.id === undefined ? "" : JSON.stringify(item.id)})`);
  }
}

/** A plugin that provides a `Contributions` registry under `key`. */
export function defineContributions<T extends Contribution = Contribution>(key: string): PluginObject<void> {
  return {
    name: key,
    provide: key,
    apply(ctx) {
      new Contributions<T>(ctx, key);
    },
  };
}

/** The items of a registry, as a computed list: empty while the registry is not provided. */
export function useContributions<T extends Contribution = Contribution>(key: string): ComputedRef<readonly T[]> {
  const registry = useService(key);
  return computed(() => (registry.value as Contributions<T> | undefined)?.items ?? []);
}
