//! The expansion of `#[derive(Service)]`.

use proc_macro2::Span;
use quote::quote;
use syn::{Data, DeriveInput, Error, LitStr, Path};

use crate::util::hyphenate;

pub(crate) fn expand_service(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
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
