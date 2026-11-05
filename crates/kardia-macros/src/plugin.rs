//! The expansion of `#[plugin]`: its arguments and the parameter type helpers.

use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{Error, Expr, ExprArray, FnArg, GenericArgument, ItemFn, Lit, LitStr, Pat, Path, PathArguments, Token, Type};

use crate::util::{camel_case, hyphenate};

#[derive(Default)]
pub(crate) struct PluginArgs {
    name: Option<LitStr>,
    provide: Vec<LitStr>,
    krate: Option<Path>,
}

impl PluginArgs {
    pub(crate) fn parse(&mut self, meta: syn::meta::ParseNestedMeta) -> syn::Result<()> {
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

pub(crate) fn expand_plugin(args: PluginArgs, item: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
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
