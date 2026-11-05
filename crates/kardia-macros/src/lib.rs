//! Procedural macros for declaring kardia plugins and services.
//!
//! - `#[derive(Service)]` binds a type to its coeffect key
//!   (`#[service(name = "database")]`, default: the hyphenated type name).
//! - `#[plugin]` turns an `async fn` into a plugin type. Taking
//!   `Injected<(A, B)>` declares `A` and `B` as dependencies, and accessing
//!   anything else through it is a compile-time error.
//!
//! Generated code refers to `::kardia_core`; pass `crate = path` to use
//! another path (e.g. `crate = ::kardia` with the facade crate).

mod plugin;
mod service;
mod util;

use proc_macro::TokenStream;
use quote::ToTokens;
use syn::{DeriveInput, Error, ItemFn, parse_macro_input};

use plugin::{PluginArgs, expand_plugin};
use service::expand_service;

/// `#[derive(Service)]`, with `#[service(name = "...", crate = path)]`.
#[proc_macro_derive(Service, attributes(service))]
pub fn derive_service(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_service(input).unwrap_or_else(Error::into_compile_error).into()
}

/// `#[plugin(name = "...", provide = ["key"], crate = path)]` on
/// `async fn f(ctx: Context | Injected<(A, ..)>, config: Arc<Config>) -> Result<()>`
/// (the config parameter is optional) generates the plugin type `F`.
#[proc_macro_attribute]
pub fn plugin(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as ItemFn);
    let mut args = PluginArgs::default();
    let parser = syn::meta::parser(|meta| args.parse(meta));
    parse_macro_input!(attr with parser);
    // on an error the function is kept, so that it reports nothing else
    let original = item.to_token_stream();
    expand_plugin(args, item)
        .unwrap_or_else(|error| {
            let mut tokens = error.into_compile_error();
            tokens.extend(original);
            tokens
        })
        .into()
}
