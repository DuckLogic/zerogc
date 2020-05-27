extern crate proc_macro;

use quote::quote;
use syn::{parse_macro_input, parse_quote, DeriveInput, Data, Error, Generics, GenericParam, TypeParamBound, Fields, Member, Index, Type, GenericArgument};
use proc_macro2::{Ident, TokenStream, Span};
use syn::spanned::Spanned;

#[proc_macro_derive(Trace)]
pub fn derive_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let trace_impl = impl_trace(&input)
        .unwrap_or_else(|e| e.to_compile_error());
    let brand_impl = impl_brand(&input)
        .unwrap_or_else(|e| e.to_compile_error());
    From::from(quote! {
        #trace_impl
        #brand_impl
    })
}

fn trace_fields(fields: &Fields, access_ref: &mut dyn FnMut(Member) -> TokenStream) -> TokenStream {
    // TODO: Detect if we're a unit struct and implement `NullTrace`
    let mut result = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let val = access_ref(match field.ident {
            Some(ref ident) => Member::Named(ident.clone()),
            None => Member::Unnamed(Index::from(index))
        });
        result.push(quote!(::zerogc::Trace::visit(#val, &mut *visitor)?));
    }
    quote!(#(#result;)*)
}

fn impl_brand(target: &DeriveInput) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let mut generics: Generics = target.generics.clone();
    let mut rewritten_params = Vec::new();
    for param in &mut generics.params {
        let rewritten_param: GenericArgument;
        match param {
            GenericParam::Type(ref mut type_param) => {
                type_param.bounds.push(parse_quote!(::zerogc::GcBrand<'new_gc, S>));
                let param_name = &type_param.ident;
                rewritten_param = parse_quote!(<#param_name as ::zerogc::GcBrand<'new_gc, S>::Branded);
            },
            GenericParam::Lifetime(ref l) => {
                /*
                 * As of right now, we can only handle at most one lifetime.
                 * That lifetime must be named `'gc` (to indicate its tied to collection).
                 */
                if l.lifetime.ident == "gc" {
                    rewritten_param = parse_quote!('new_gc);
                } else {
                    return Err(Error::new(
                        l.span(),
                        "Only allowed to have 'gc lifetime"
                    ));
                }
            },
            GenericParam::Const(ref param) => {
                let name = &param.ident;
                rewritten_param = GenericArgument::Const(parse_quote!(#name));
            }
        }
        rewritten_params.push(rewritten_param);
    }
    let mut impl_generics = generics.clone();
    impl_generics.params.push(GenericParam::Lifetime(parse_quote!('new_gc)));
    impl_generics.params.push(GenericParam::Type(parse_quote!(S: ::zerogc::GcSystem)));
    let (_, ty_generics, where_clause) = generics.split_for_impl();
    let (impl_generics, _, _) = impl_generics.split_for_impl();
    Ok(quote! {
        unsafe impl #impl_generics ::zerogc::GcBrand<'new_gc, S>
            for #name #ty_generics #where_clause {
            type Branded = #name::<#(#rewritten_params),*>;
        }
    })
}
fn impl_trace(target: &DeriveInput) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let generics = add_trait_bounds(
        &target.generics, parse_quote!(zerogc::Trace)
    );
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let field_types: Vec<&Type>;
    let trace_impl: TokenStream;
    match target.data {
        Data::Struct(ref data) => {
            trace_impl = trace_fields(
                &data.fields,
                &mut |member| quote!(&mut self.#member)
            );
            field_types = data.fields.iter().map(|f| &f.ty).collect();
        },
        Data::Enum(ref data) => {
            field_types = data.variants.iter()
                .flat_map(|var| var.fields.iter().map(|f| &f.ty))
                .collect();
            let mut match_arms = Vec::new();
            for variant in &data.variants {
                let variant_name = &variant.ident;
                let trace_variant = trace_fields(
                    &variant.fields,
                    &mut |member| {
                        let ident = match member {
                            Member::Named(name) => name,
                            Member::Unnamed(index) => {
                                Ident::new(
                                    &format!("field{}", index.index),
                                    index.span
                                )
                            },
                        };
                        quote!(ref mut #ident)
                    }
                );
                let pattern = match variant.fields {
                    Fields::Named(ref fields) => {
                        let names = fields.named.iter()
                            .map(|field| field.ident.as_ref().unwrap());
                        quote!({ #(#names,)* })
                    },
                    Fields::Unnamed(ref fields) => {
                        let names = (0..fields.unnamed.len())
                            .map(|index| Ident::new(
                                &format!("field{}", index),
                                Span::call_site()
                            ));
                        quote!(#(#names,)*)
                    },
                    Fields::Unit => quote!(),
                };
                match_arms.push(quote!(#name::#variant_name #pattern => { #trace_variant }));
            }
            trace_impl = quote!(match self {
                #(#match_arms,)*
            });
        },
        Data::Union(_) => {
            return Err(Error::new(
                name.span(),
                "Unions can't be automatically traced"
            ));
        },
    }
    Ok(quote! {
        unsafe impl #impl_generics ::zerogc::Trace for #name #ty_generics #where_clause {
            const NEEDS_TRACE: bool = false #(|| <#field_types as ::zerogc::Trace>::NEEDS_TRACE)*;

            /*
             * The inline annotation adds this function's MIR to the metadata.
             * Without it cross-crate inlining is impossible (without LTO).
             */
            #[inline]
            fn visit<V: ::zerogc::GcVisitor>(&mut self, #[allow(unused)] visitor: &mut V) -> Result<(), V::Err> {
                #trace_impl
                Ok(())
            }
        }
    })
}

fn add_trait_bounds(generics: &Generics, bound: TypeParamBound) -> Generics {
    let mut result: Generics = (*generics).clone();
    for param in &mut result.params {
        if let GenericParam::Type(ref mut type_param) = *param {
            type_param.bounds.push(bound.clone());
        }
    }
    result
}
