import type { Context } from "@kardiajs/core";
import { type Component, defineComponent, h, type PropType } from "vue";

import { provideContext, useService } from "#/context";
import type { Contribution, Contributions } from "#/contributions";

/** A contribution that is a component: what an `<ExtensionSlot>` renders. */
export interface ComponentContribution extends Contribution {
  component: Component;
  /** Props for this component, over the slot's `props`. */
  props?: Record<string, unknown>;
}

/** Renders a subtree with its owner's context, so its effects belong to the contributing plugin. */
const Owned = defineComponent({
  name: "KardiaOwned",
  props: { owner: { type: Object as PropType<Context>, required: true } },
  setup(props, { slots }) {
    provideContext(props.owner);
    return () => slots.default?.();
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
 */
export const ExtensionSlot = defineComponent({
  name: "ExtensionSlot",
  props: {
    name: { type: String, required: true },
    props: { type: Object as PropType<Record<string, unknown>>, default: () => ({}) },
  },
  setup(props, { slots }) {
    const registry = useService(props.name);
    return () => {
      const entries = (registry.value as Contributions<ComponentContribution> | undefined)?.entries ?? [];
      if (!entries.length) return slots.default?.();
      return entries.map(({ item, owner }, index) =>
        h(Owned, { key: item.id ?? index, owner }, () =>
          slots.item ? slots.item({ item, owner }) : h(item.component, { ...props.props, ...item.props }),
        ),
      );
    };
  },
});
