import type { Context } from "@kardiajs/core";
import { type Component, defineComponent, h, onErrorCaptured, type PropType, shallowRef } from "vue";

import { provideContext, useService } from "#/context";
import type { Contribution, Contributions } from "#/contributions";

/** A contribution that is a component: what an `<ExtensionSlot>` renders. */
export interface ComponentContribution extends Contribution {
  component: Component;
  /** Props for this component, over the slot's `props`. */
  props?: Record<string, unknown>;
}

/** A contribution that failed in an `<ExtensionSlot>`, with the plugin that contributed it. */
export interface ContributionError {
  readonly error: unknown;
  readonly item: ComponentContribution;
  readonly owner: Context;
}

/**
 * Renders a subtree with its owner's context, so its effects belong to the
 * contributing plugin, and contains its errors: a failure replaces the
 * subtree with the `error` slot until `retry` mounts it again.
 */
const Owned = defineComponent({
  name: "KardiaOwned",
  props: {
    owner: { type: Object as PropType<Context>, required: true },
    item: { type: Object as PropType<ComponentContribution>, required: true },
    onError: Function as PropType<(failure: ContributionError) => void>,
  },
  setup(props, { slots }) {
    provideContext(props.owner);
    const failure = shallowRef<ContributionError>();
    const retry = () => void (failure.value = undefined);
    onErrorCaptured((error) => {
      // a failing fallback is the host's error: it propagates instead of looping
      if (failure.value) return;
      failure.value = { error, item: props.item, owner: props.owner };
      if (props.onError) props.onError(failure.value);
      else props.owner.logger().error(error);
      return false;
    });
    return () => (failure.value ? slots.error?.({ ...failure.value, retry }) : slots.default?.());
  },
});

/**
 * Renders the components contributed to a `Contributions` registry, in order.
 * Each one gets the context of the plugin that contributed it, so its
 * `useContext()`, `usePlugin()` and `useService()` act as that plugin.
 *
 * - `name`: the registry's key (read once).
 * - `props`: passed to every component, under each contribution's own `props`
 *   (a component that does not declare one gets it as an attribute, as usual in Vue).
 * - default slot: rendered while there is nothing to show.
 * - `item` slot (`{ item, owner }`): renders each contribution instead of its component.
 * - `error` slot (`{ error, item, owner, retry }`): replaces a contribution that
 *   threw (in setup, render, a hook, a watcher or a handler); `retry()` mounts
 *   it again. Without it, a failed contribution renders nothing.
 * - `onError` (`@error`, `{ error, item, owner }`): reports a failure; without
 *   it, the failure is logged by the owner's logger. Either way it does not
 *   reach the app, and the owner's plugin keeps running.
 */
export const ExtensionSlot = defineComponent({
  name: "ExtensionSlot",
  props: {
    name: { type: String, required: true },
    props: { type: Object as PropType<Record<string, unknown>>, default: () => ({}) },
    onError: Function as PropType<(failure: ContributionError) => void>,
  },
  setup(props, { slots }) {
    const registry = useService(props.name);
    return () => {
      const entries = (registry.value as Contributions<ComponentContribution> | undefined)?.entries ?? [];
      if (!entries.length) return slots.default?.();
      return entries.map(({ item, owner }, index) =>
        h(
          Owned,
          { key: item.id ?? index, owner, item, onError: props.onError },
          {
            default: () =>
              slots.item ? slots.item({ item, owner }) : h(item.component, { ...props.props, ...item.props }),
            error: slots.error,
          },
        ),
      );
    };
  },
});
