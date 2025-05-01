extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{Expr, Path, Result, parse_macro_input};

struct ConcatInput {
    crate_path: Path,
    args: Punctuated<Expr, Comma>,
}

impl Parse for ConcatInput {
    fn parse(input: ParseStream) -> Result<Self> {
        let crate_path: Path = input.parse()?;
        input.parse::<Comma>()?;
        Ok(ConcatInput {
            crate_path,
            args: Punctuated::parse_terminated(input)?,
        })
    }
}

#[proc_macro]
pub fn __concat_helper(input: TokenStream) -> TokenStream {
    let ConcatInput { args, crate_path } = parse_macro_input!(input as ConcatInput);
    let arg_idents: Vec<_> = (0..args.len())
        .map(|i| syn::Ident::new(&format!("arg{i}"), Span::mixed_site()))
        .collect();

    let fn_args = arg_idents.iter().map(|ident| {
        quote! { #ident: impl #crate_path::WriteToMessage }
    });

    let fn_body = arg_idents.iter().map(|ident| {
        quote! { #ident.write_to(builder)?; }
    });

    let call_args = args.iter();

    quote! {
        ({
            #[allow(clippy::too_many_arguments)]
            const fn concat(#(#fn_args),*) -> impl #crate_path::WriteToMessage {
                #crate_path::from_fn(move |builder| {
                    #(#fn_body)*
                    Ok(())
                })
            }

            concat
        })(#(#call_args),*)
    }
    .into()
}
