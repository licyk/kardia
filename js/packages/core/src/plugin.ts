import type { Context } from "./context.js";
import { type Issue, ValidationError } from "./errors.js";

/**
 * The Standard Schema interface (https://standardschema.dev), copied as its
 * authors intend, so any compliant validator (zod, valibot, arktype, ...) can
 * validate a plugin's configuration without a dependency.
 */
export interface StandardSchemaV1<Input = unknown, Output = Input> {
  readonly "~standard": {
    readonly version: 1;
    readonly vendor: string;
    readonly validate: (
      value: unknown,
    ) => StandardSchemaV1.Result<Output> | Promise<StandardSchemaV1.Result<Output>>;
    readonly types?: { readonly input: Input; readonly output: Output } | undefined;
  };
}

export declare namespace StandardSchemaV1 {
  type Result<Output> = { readonly value: Output; readonly issues?: undefined } | { readonly issues: readonly Issue[] };
}

/** Declared dependencies: keys, or keys mapped to interception metadata. */
export type InjectSpec<D extends string = string> = readonly D[] | { readonly [K in D]?: unknown };

export interface PluginMeta<C = any> {
  /** The plugin's name, used for loggers and diagnostics. Defaults to the function or class name. */
  name?: string;
  /** Services the plugin depends on: it runs while all of them are available. */
  inject?: InjectSpec;
  /** Services the plugin provides. Metadata only: used for the dependency-cycle check. */
  provide?: string | readonly string[];
  /** Validates and transforms the configuration before `apply` sees it. */
  Config?: StandardSchemaV1<unknown, C>;
}

export type PluginFunction<C = any, D extends string = any> = PluginMeta<C> &
  ((ctx: Context<D>, config: C) => unknown);

export type PluginClass<C = any, D extends string = any> = PluginMeta<C> & (new (ctx: Context<D>, config: C) => unknown);

export interface PluginObject<C = any, D extends string = any> extends PluginMeta<C> {
  apply(ctx: Context<D>, config: C): unknown;
}

/**
 * A plugin: a function, a class (instantiated per fiber) or an object with
 * `apply`. `apply` may return a promise (the plugin is loading until it
 * settles) or a function (run when the plugin stops).
 */
export type Plugin<C = any, D extends string = any> = PluginFunction<C, D> | PluginClass<C, D> | PluginObject<C, D>;

/** The configuration a plugin accepts. */
export type ConfigOf<P> = P extends { Config: StandardSchemaV1<infer I, any> }
  ? I
  : P extends new (ctx: any, config: infer C) => any
    ? C
    : P extends (ctx: any, config: infer C) => any
      ? C
      : P extends { apply(ctx: any, config: infer C): any }
        ? C
        : unknown;

/**
 * Defines a plugin object whose `apply` sees a context typed by its `inject`:
 * accessing an undeclared service is a type error.
 */
export function definePlugin<const D extends string = never, C = unknown>(
  plugin: PluginMeta<C> & { inject?: InjectSpec<D>; apply(ctx: Context<D>, config: C): unknown },
): PluginObject<C, D> {
  return plugin as PluginObject<C, D>;
}

/** A plugin normalized for the runtime. Its identity is the plugin value. */
export interface PluginDef {
  readonly key: object;
  readonly name: string | undefined;
  readonly inject: ReadonlyMap<string, unknown>;
  readonly provide: readonly string[];
  readonly apply: (ctx: Context, config: unknown) => unknown;
  decode(config: unknown): unknown;
}

const defs = new WeakMap<object, PluginDef>();

/** Normalizes a plugin value; the same value always yields the same definition. */
export function resolvePlugin(plugin: Plugin): PluginDef {
  let def = defs.get(plugin);
  if (def) return def;
  def = createDef(plugin);
  defs.set(plugin, def);
  return def;
}

function createDef(plugin: Plugin): PluginDef {
  const apply = applyOf(plugin);
  const meta = plugin as PluginMeta;
  const schema = meta.Config;
  const name = typeof meta.name === "string" && meta.name && meta.name !== "apply" ? meta.name : undefined;
  return {
    key: plugin,
    name,
    inject: normalizeInject(meta.inject),
    provide: typeof meta.provide === "string" ? [meta.provide] : [...(meta.provide ?? [])],
    apply,
    decode: (config) => (schema ? validate(schema, config) : config),
  };
}

function applyOf(plugin: Plugin): PluginDef["apply"] {
  if (typeof plugin === "function") {
    if (isClass(plugin)) {
      const Class = plugin as new (ctx: Context, config: unknown) => unknown;
      return (ctx, config) => {
        new Class(ctx, config);
      };
    }
    return plugin as (ctx: Context, config: unknown) => unknown;
  }
  if (plugin && typeof plugin === "object" && typeof plugin.apply === "function") {
    return (ctx, config) => plugin.apply(ctx, config);
  }
  throw new TypeError(`invalid plugin, expect function or object with an "apply" method, received ${typeof plugin}`);
}

function isClass(value: Function) {
  return /^class[\s{]/.test(Function.prototype.toString.call(value));
}

export function normalizeInject(inject: InjectSpec | undefined): Map<string, unknown> {
  const result = new Map<string, unknown>();
  if (!inject) return result;
  if (Array.isArray(inject)) {
    for (const key of inject as readonly string[]) result.set(key, undefined);
  } else {
    for (const [key, meta] of Object.entries(inject)) result.set(key, meta ?? undefined);
  }
  return result;
}

function validate(schema: StandardSchemaV1, config: unknown) {
  const result = schema["~standard"].validate(config);
  if (result instanceof Promise) throw new TypeError("asynchronous config validation is not supported");
  if (result.issues) throw new ValidationError(result.issues);
  return result.value;
}
