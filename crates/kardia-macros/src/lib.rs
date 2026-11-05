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

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{ToTokens, format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Data, DeriveInput, Error, Expr, ExprArray, FnArg, GenericArgument, ItemFn, Lit, LitStr, Pat, Path, PathArguments,
    Token, Type, parse_macro_input,
};

/// `#[derive(Service)]`, with `#[service(name = "...", crate = path)]`.
#[proc_macro_derive(Service, attributes(service))]
pub fn derive_service(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_service(input).unwrap_or_else(Error::into_compile_error).into()
}

fn expand_service(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if let Data::Union(_) = input.data {
        return Err(Error::new_spanned(&input.ident, "a service must be a struct or an enum"));
    }
    let mut name = None;
    let mut krate: Path = syn::parse_quote!(::kardia_core);
    for attr in input.attrs.iter().filter(|attr| attr.path().is_ident("service")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                name = Some(meta.value()?.parse::<LitStr>()?);
            } else if meta.path.is_ident("crate") {
                krate = meta.value()?.parse()?;
            } else {
                return Err(meta.error("expected `name` or `crate`"));
            }
            Ok(())
        })?;
    }
    let ident = &input.ident;
    let name = name.unwrap_or_else(|| LitStr::new(&hyphenate(&ident.to_string()), Span::call_site()));
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #krate::Service for #ident #type_generics #where_clause {
            const NAME: &'static str = #name;
        }
    })
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

#[derive(Default)]
struct PluginArgs {
    name: Option<LitStr>,
    provide: Vec<LitStr>,
    krate: Option<Path>,
}

impl PluginArgs {
    fn parse(&mut self, meta: syn::meta::ParseNestedMeta) -> syn::Result<()> {
        if meta.path.is_ident("name") {
            self.name = Some(meta.value()?.parse()?);
        } else if meta.path.is_ident("provide") {
            let array: ExprArray = meta.value()?.parse()?;
            self.provide = array
                .elems
                .into_iter()
                .map(|elem| match elem {
                    Expr::Lit(syn::ExprLit { lit: Lit::Str(key), .. }) => Ok(key),
                    other => Err(Error::new_spanned(other, "expected a string literal")),
                })
                .collect::<syn::Result<_>>()?;
        } else if meta.path.is_ident("crate") {
            self.krate = Some(meta.value()?.parse()?);
        } else {
            return Err(meta.error("expected `name`, `provide` or `crate`"));
        }
        Ok(())
    }
}

fn expand_plugin(args: PluginArgs, item: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    let sig = &item.sig;
    if sig.asyncness.is_none() {
        return Err(Error::new_spanned(sig.fn_token, "a plugin function must be `async`"));
    }
    let krate = args.krate.unwrap_or_else(|| syn::parse_quote!(::kardia_core));
    let inputs: Vec<&Type> = sig
        .inputs
        .iter()
        .map(|input| match input {
            FnArg::Typed(typed) if matches!(*typed.pat, Pat::Ident(_) | Pat::Wild(_)) => Ok(&*typed.ty),
            other => Err(Error::new_spanned(
                other,
                "expected `ctx: Context | Injected<..>` and an optional `config: Arc<T>`",
            )),
        })
        .collect::<syn::Result<_>>()?;
    let (context, config) = match inputs.as_slice() {
        [context] => (*context, None),
        [context, config] => (*context, Some(arc_inner(config)?)),
        _ => return Err(Error::new_spanned(&sig.inputs, "expected one or two parameters")),
    };
    let fn_name = &sig.ident;
    let plugin = format_ident!("{}", camel_case(&fn_name.to_string()));
    let name = args.name.unwrap_or_else(|| LitStr::new(&hyphenate(&fn_name.to_string()), fn_name.span()));
    let vis = &item.vis;
    let (inject, context_arg) = match injected_deps(context) {
        Some(deps) => (quote!(#krate::Inject::of::<#deps>()), quote!(#krate::Injected::<#deps>::new(ctx))),
        None => (quote!(#krate::Inject::new()), quote!(ctx)),
    };
    let config_type = config.map_or_else(|| quote!(()), |config| quote!(#config));
    let config_arg = config.map(|_| quote!(, config));
    let provide = &args.provide;
    Ok(quote! {
        #vis struct #plugin;

        impl #krate::Plugin for #plugin {
            const NAME: &'static str = #name;
            type Config = #config_type;

            fn inject() -> #krate::Inject {
                #inject
            }

            fn provide() -> ::std::vec::Vec<#krate::Key> {
                ::std::vec![#(#krate::Key::from(#provide)),*]
            }

            async fn apply(ctx: #krate::Context, config: ::std::sync::Arc<#config_type>) -> #krate::Result<()> {
                let _ = &config;
                #fn_name(#context_arg #config_arg).await
            }
        }

        #item
    })
}

/// `D` of `Injected<D>`.
fn injected_deps(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    if segment.ident != "Injected" {
        return None;
    }
    generic_argument(&segment.arguments)
}

/// `T` of `Arc<T>`.
fn arc_inner(ty: &Type) -> syn::Result<&Type> {
    let error = || Error::new_spanned(ty, "the config parameter must be an `Arc<Config>`");
    let Type::Path(path) = ty else { return Err(error()) };
    let segment = path.path.segments.last().ok_or_else(error)?;
    if segment.ident != "Arc" {
        return Err(error());
    }
    generic_argument(&segment.arguments).ok_or_else(error)
}

fn generic_argument(arguments: &PathArguments) -> Option<&Type> {
    let PathArguments::AngleBracketed(arguments) = arguments else { return None };
    let args: &Punctuated<GenericArgument, Token![,]> = &arguments.args;
    match args.first()? {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    }
}

/// `db_plugin` → `DbPlugin`.
fn camel_case(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
        })
        .collect()
}

/// `DbPlugin` / `db_plugin` → `db-plugin`.
fn hyphenate(name: &str) -> String {
    let mut result = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch == '_' {
            result.push('-');
        } else if ch.is_ascii_uppercase() {
            if index > 0 && !result.ends_with('-') {
                result.push('-');
            }
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push(ch);
        }
    }
    result
}
