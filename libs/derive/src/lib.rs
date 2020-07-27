extern crate proc_macro;

use quote::{quote, quote_spanned};
use syn::{
    parse_macro_input, parenthesized, parse_quote, DeriveInput, Data,
    Error, Generics, GenericParam, TypeParamBound, Fields, Member,
    Index, Type, GenericArgument, Attribute, PathArguments,
};
use proc_macro2::{Ident, TokenStream, Span};
use syn::spanned::Spanned;
use syn::parse::{ParseStream, Parse};

struct MutableFieldOpts {
    public: bool
}
impl Default for MutableFieldOpts {
    fn default() -> MutableFieldOpts {
        MutableFieldOpts {
            public: false, // Privacy by default
        }
    }
}
impl Parse for MutableFieldOpts {
    fn parse(input: ParseStream) -> Result<Self, Error> {
        let mut result = MutableFieldOpts::default();
        while !input.is_empty() {
            let flag_name = input.parse::<Ident>()?;
            if flag_name == "public" {
                result.public = true;
            } else {
                return Err(Error::new(
                    input.span(),
                    "Unknown modifier for mutable field"
                ))
            }
        }
        Ok(result)
    }
}

struct GcFieldAttrs {
    mutable: Option<MutableFieldOpts>
}
impl GcFieldAttrs {
    pub fn find(attrs: &[Attribute]) -> Result<Self, Error> {
        attrs.iter().find_map(|attr| {
            if attr.path.is_ident("zerogc") {
                Some(syn::parse2::<GcFieldAttrs>(attr.tokens.clone()))
            } else {
                None
            }
        }).unwrap_or_else(|| Ok(GcFieldAttrs::default()))
    }
}
impl Default for GcFieldAttrs {
    fn default() -> Self {
        GcFieldAttrs {
            mutable: None
        }
    }
}
impl Parse for GcFieldAttrs {
    fn parse(raw_input: ParseStream) -> Result<Self, Error> {
        let input;
        parenthesized!(input in raw_input);
        let mut result = GcFieldAttrs::default();
        while !input.is_empty() {
            let flag_name = input.parse::<Ident>()?;
            if flag_name == "mutable" {
                if input.peek(syn::token::Paren) {
                    let mut_opts;
                    parenthesized!(mut_opts in input);
                    result.mutable = Some(mut_opts.parse::<MutableFieldOpts>()?);
                    if !input.is_empty() {
                        return Err(Error::new(
                            input.span(), "Unexpected input"
                        ));
                    }
                } else {
                    result.mutable = Some(MutableFieldOpts::default());
                }
            } else {
                return Err(Error::new(
                    input.span(), "Unknown field flag"
                ))
            }
        }
        Ok(result)
    }
}

struct GcTypeAttrs {
    is_copy: bool,
}
impl GcTypeAttrs {
    pub fn find(attrs: &[Attribute]) -> Result<Self, Error> {
        attrs.iter().find_map(|attr| {
            if attr.path.is_ident("zerogc") {
                Some(syn::parse2::<GcTypeAttrs>(attr.tokens.clone()))
            } else {
                None
            }
        }).unwrap_or_else(|| Ok(GcTypeAttrs::default()))
    }
}
impl Default for GcTypeAttrs {
    fn default() -> Self {
        GcTypeAttrs {
            is_copy: false,
        }
    }
}
impl Parse for GcTypeAttrs {
    fn parse(raw_input: ParseStream) -> Result<Self, Error> {
        let input;
        parenthesized!(input in raw_input);
        let mut result = GcTypeAttrs::default();
        while !input.is_empty() {
            let flag_name = input.parse::<Ident>()?;
            if flag_name == "copy" {
                result.is_copy = true;
            } else {
                return Err(Error::new(
                    input.span(), "Unknown type flag"
                ))
            }
        }
        Ok(result)
    }
}

#[proc_macro_derive(Trace, attributes(zerogc))]
pub fn derive_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let trace_impl = impl_trace(&input)
        .unwrap_or_else(|e| e.to_compile_error());
    let brand_impl = impl_brand(&input)
        .unwrap_or_else(|e| e.to_compile_error());
    let gc_safe_impl = impl_gc_safe(&input)
        .unwrap_or_else(|e| e.to_compile_error());
    let extra_impls = impl_extras(&input)
        .unwrap_or_else(|e| e.to_compile_error());
    From::from(quote! {
        #trace_impl
        #brand_impl
        #gc_safe_impl
        #extra_impls
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

/// Implement extra methods
///
/// 1. Implement setters for `GcCell` fields using a write barrier
fn impl_extras(target: &DeriveInput) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let mut extra_items = Vec::new();
    match target.data {
        Data::Struct(ref data) => {
            for field in &data.fields {
                let attrs = GcFieldAttrs::find(&field.attrs)?;
                if let Some(mutable_opts) = attrs.mutable {
                    let original_name = match field.ident {
                        Some(ref name) => name,
                        None => {
                            return Err(Error::new(
                                field.span(),
                                "zerogc can only mutate named fields"
                            ))
                        }
                    };
                    // Generate a mutator
                    let mutator_name = Ident::new(
                        &format!("set_{}", original_name),
                        field.ident.span(),
                    );
                    let mutator_vis = if mutable_opts.public {
                        quote!(pub)
                    } else {
                        quote!()
                    };
                    let value_ref_type = match field.ty {
                        Type::Path(ref cell_path) if cell_path.path.segments.last()
                            .map_or(false, |seg| seg.ident == "GcCell") => {
                            let last_segment = cell_path.path.segments.last().unwrap();
                            let mut inner_type = None;
                            if let PathArguments::AngleBracketed(ref bracketed) = last_segment.arguments {
                                for arg in &bracketed.args {
                                    match arg {
                                        GenericArgument::Type(t) if inner_type.is_none() => {
                                            inner_type = Some(t.clone()); // Initialize
                                        },
                                        _ => {
                                            inner_type = None; // Unexpected arg
                                            break
                                        }
                                    }
                                }
                            }
                            inner_type.ok_or_else(|| Error::new(
                                field.ty.span(),
                                "GcCell should have one (and only one) type param"
                            ))?
                        },
                        _ => return Err(Error::new(
                            field.ty.span(),
                            "A mutable field must be wrapped in a `GcCell`"
                        ))
                    };
                    // NOTE: Specially quoted since we want to blame the field for errors
                    let field_as_ptr = quote_spanned!(field.span() => GcCell::as_ptr(&(*self.value()).#original_name));
                    let barrier = quote_spanned!(field.span() => ::zerogc::GcDirectBarrier::write_barrier(&value, &self, offset));
                    extra_items.push(quote! {
                        #[inline] // TODO: Implement `GcDirectBarrier` ourselves
                        #mutator_vis fn #mutator_name<OwningRef>(self: OwningRef, value: #value_ref_type)
                            where OwningRef: ::zerogc::GcRef<'gc, Self>,
                                   #value_ref_type: ::zerogc::GcDirectBarrier<'gc, OwningRef> {
                            unsafe {
                                let target_ptr = #field_as_ptr;
                                let offset = target_ptr as usize - self.as_raw_ptr() as usize;
                                #barrier;
                                target_ptr.write(value);
                            }
                        }
                    })
                }
            }
        },
        Data::Enum(ref data) => {
            for variant in &data.variants {
                for field in &variant.fields {
                    let attrs = GcFieldAttrs::find(&field.attrs)?;
                    if attrs.mutable.is_some() {
                        return Err(Error::new(
                            field.ident.span(),
                            "Can't mark enum field as mutable"
                        ))
                    }
                }
            }
        },
        Data::Union(ref data) => {
            for field in &data.fields.named {
                let attrs = GcFieldAttrs::find(&field.attrs)?;
                if attrs.mutable.is_some() {
                    return Err(Error::new(
                        field.ident.span(),
                        "Can't mark union field as mutable"
                    ))
                }
            }
        },
    }
    let (impl_generics, ty_generics, where_clause) = target.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            #(#extra_items)*
        }
    })
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
fn impl_gc_safe(target: &DeriveInput) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let generics = add_trait_bounds(
        &target.generics, parse_quote!(zerogc::GcSafe)
    );
    let attrs = GcTypeAttrs::find(&*target.attrs)?;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let field_types: Vec<&Type> = match target.data {
        Data::Struct(ref data) => {
            data.fields.iter()
                .map(|f| &f.ty)
                .collect()
        },
        Data::Enum(ref data) => {
            data.variants.iter()
                .flat_map(|v| v.fields.iter().map(|f| &f.ty))
                .collect()
        },
        Data::Union(_) => {
            return Err(Error::new(
                name.span(), "Unions are unsupported for GcSafe"
            ))
        },
    };
    let does_need_drop = if attrs.is_copy {
        // If we're proven to be a copy type we don't need to be dropped
        quote!(false)
    } else {
        // We need to be dropped if any of our fields need to be dropped
        quote!(false #(|| <#field_types as ::zerogc::GcSafe>::NEEDS_DROP)*)
    };
    let fake_drop_impl = if attrs.is_copy {
        /*
         * If this type can be proven to implement Copy,
         * it is known to have no destructor.
         * We don't need a fake drop to prevent misbehavior.
         * In fact, adding one would prevent it from being Copy
         * in the first place.
         */
        quote!()
    } else {
        quote!(impl #impl_generics Drop for #name #ty_generics #where_clause {
            #[inline]
            fn drop(&mut self) {
                /*
                 * This is only here to prevent the user
                 * from implementing their own Drop functionality.
                 */
            }
        })
    };
    let verify_gc_safe = if attrs.is_copy {
        quote!(::zerogc::assert_copy::<Self>())
    } else {
        quote!(#(<#field_types as GcSafe>::assert_gc_safe();)*)
    };
    Ok(quote! {
        unsafe impl #impl_generics ::zerogc::GcSafe
            for #name #ty_generics #where_clause {
            const NEEDS_DROP: bool = #does_need_drop;

            fn assert_gc_safe() {
                #verify_gc_safe
            }
        }
        #fake_drop_impl
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
