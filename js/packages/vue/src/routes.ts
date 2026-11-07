import type { Context, EffectHandle } from "@kardiajs/core";

/** What `addRoute` needs of a router: vue-router's `addRoute`, which returns the route's remover. */
export interface RouteHost<R> {
  addRoute(route: R): () => void;
}

/** Adds a route as an effect of `ctx`'s plugin: it is removed when that plugin stops. */
export function addRoute<R>(ctx: Context, router: RouteHost<R>, route: R): EffectHandle {
  return ctx.effect(() => router.addRoute(route), "router.addRoute()");
}
