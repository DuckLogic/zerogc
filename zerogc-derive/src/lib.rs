extern crate  proc_macro;

use syn::{parse_macro_input, DeriveInput, Data, GenericParam, LifetimeDef, ImplGenerics, TypeGenerics, WhereClause, Generics, WherePredicate};
use syn::spanned::Spanned;
use quote::quote;
use proc_macro2::Span;

pub struct GcGenerics {
    original_generics: Generics,
    without_gc_lifetime: Generics
}
impl GcGenerics {
    fn new(generics: Generics) -> Self {
        let without_gc_lifetime = generics.params.iter()
            .filter(|param| match param {
                GenericParam::Lifetime(ref lifetime) if lifetime.lifetime.ident == "gc" => false,
                _ => true
            }).collect();
        let mut without_gc_lifetime = generics.clone();
        without_gc_lifetime.params = params_without_gc_lifetime;
        GcGenerics {
            original_generics: generics,
            without_gc_lifetime
        }
    }
    fn extend_generics(&self, extra: &[&str], where_clauses: &[WherePredicate]) -> Generics {
        let (_, _, where_clause) = self.without_gc_lifetime();
        let generics = self.without_gc_lifetime.clone();
        for lifetime in extra {
            generics.params.push(GenericParam::Lifetime(LifetimeDef::new(
                Lifetime::new(lifetime, Span::call_site())
            )));
        }
        {
            let where_clause = generics.where_clause.get_or_insert_with(|| WhereClause {
                predicates: Default::default(),
                where_token: Default::default()
            });
            for clause in where_clauses {
                where_clause.predicates.push(clause)
            }
        }
        generics
    }
}

#[derive(Gc)]
pub fn derive_gc(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: DeriveInput = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let generics = GcGenerics::new(input.generics.clone());
    let (impl_generics, ty_generics, where_clause) = generics.original_generics.split_for_impl();
    let mut extra_where_predicates = Vec::new();
    for param in generic.without_gc_lifetime {

    }
    match input.data {
        Data::Struct(ref data) => {
            let mut trace_fields = Vec::new();
            for (index, field) in data.fields.enumerate() {
                let access = match field.ident {
                    Some(name) => quote!(self.#name),
                    None => {
                        let index = syn::Index::from(index);
                        quote!(self.#index)
                    }
                };
                trace_fields.push(quote!(target.trace(#access);))
            }
            let erased_impl_generics = generics.declare_extra_lifetimes(&["gc", "unm"]);
            quote! {
                impl #impl_generics zerogc::GarbageCollected for #name #ty_generics #where_clause {
                    unsafe fn raw_trace(&self, target: &mut GarbageCollector) {
                        #(#trace_fields)
                    }
                }
                impl #impl_generics z
            }
        },
        Data::Enum(_) => {
            return syn::Error::new(name.span(), "Enums are unsupported")
                .to_compile_error().into()
        },
        Data::Union(_) => {
            return syn::Error::new(name.span(), "Unions are unsupported")
                .to_compile_error().into()
        },
    }.into()
}
