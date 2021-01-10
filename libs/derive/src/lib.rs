extern crate proc_macro;

use quote::{quote, quote_spanned};
use syn::{parse_macro_input, parenthesized, parse_quote, DeriveInput, Data, Error, Generics, GenericParam, TypeParamBound, Fields, Member, Index, Type, GenericArgument, Attribute, PathArguments, Meta, TypeParam, WherePredicate, PredicateType, Token, Lifetime, NestedMeta, Lit, PredicateLifetime};
use proc_macro2::{Ident, TokenStream, Span};
use syn::spanned::Spanned;
use syn::parse::{ParseStream, Parse};
use std::collections::HashSet;
use std::fmt::Display;
use std::io::Write;

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
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
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
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(result)
    }
}

struct GcTypeInfo {
    config: TypeAttrs,
}
impl GcTypeInfo {
    fn parse(input: &DeriveInput) -> Result<GcTypeInfo, Error> {
        let config = TypeAttrs::find(&*input.attrs)?;
        if config.ignored_lifetimes.contains(&config.gc_lifetime()) {
            return Err(Error::new(
                config.gc_lifetime().span(),
                "Ignored gc lifetime"
            ))
        }
        Ok(GcTypeInfo { config })
    }
}
struct TypeAttrs {
    is_copy: bool,
    nop_trace: bool,
    gc_lifetime: Option<Lifetime>,
    ignore_params: HashSet<Ident>,
    ignored_lifetimes: HashSet<Lifetime>
}
impl TypeAttrs {
    fn gc_lifetime(&self) -> Lifetime {
        match self.gc_lifetime {
            Some(ref lt) => lt.clone(),
            None => Lifetime::new("'gc", Span::call_site())
        }
    }
    pub fn find(attrs: &[Attribute]) -> Result<Self, Error> {
        attrs.iter().find_map(|attr| {
            if attr.path.is_ident("zerogc") {
                Some(syn::parse2::<TypeAttrs>(attr.tokens.clone()))
            } else {
                None
            }
        }).unwrap_or_else(|| Ok(TypeAttrs::default()))
    }
}
impl Default for TypeAttrs {
    fn default() -> Self {
        TypeAttrs {
            is_copy: false,
            nop_trace: false,
            gc_lifetime: None,
            ignore_params: Default::default(),
            ignored_lifetimes: Default::default(),
        }
    }
}
impl Parse for TypeAttrs {
    fn parse(raw_input: ParseStream) -> Result<Self, Error> {
        let input;
        parenthesized!(input in raw_input);
        let mut result = TypeAttrs::default();
        while !input.is_empty() {
            let meta = input.parse::<Meta>()?;
            if meta.path().is_ident("copy") {
                if !matches!(meta, Meta::Path(_)) {
                    return Err(Error::new(
                        meta.span(),
                        "Malformed attribute for #[zerogc(copy)]"
                    ))
                }
                if result.is_copy {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(copy)]"
                    ))
                }
                result.is_copy = true;
            } else if meta.path().is_ident("nop_trace") {
                if !matches!(meta, Meta::Path(_)) {
                    return Err(Error::new(
                        meta.span(),
                        "Malformed attribute for #[zerogc(nop_trace)]"
                    ))
                }
                if result.nop_trace {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(nop_trace)]"
                    ))
                }
                result.nop_trace = true;
            } else if meta.path().is_ident("gc_lifetime") {
                if result.gc_lifetime.is_some() {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(gc_lifetime)]"
                    ))
                }
                let s = match meta {
                    Meta::NameValue(syn::MetaNameValue {
                        lit: Lit::Str(ref s), ..
                    }) => s,
                    _ => {
                        return Err(Error::new(
                            meta.span(),
                            "Malformed attribute for #[zerogc(nop_trace)]"
                        ))
                    }
                };
                let lifetime = match s.parse::<::syn::Lifetime>() {
                    Ok(lifetime) => lifetime,
                    Err(cause) => {
                        return Err(Error::new(s.span(), format_args!(
                            "Invalid lifetime name for #[zerogc(gc_lifetime)]: {}",
                            cause
                        )));
                    }
                };
                result.gc_lifetime = Some(lifetime);
            } else if meta.path().is_ident("ignore_params") {
                if !result.ignore_params.is_empty() {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(ignore_params)]"
                    ))
                }
                let list = match meta {
                    Meta::List(ref list) if list.nested.is_empty() => {
                        return Err(Error::new(
                            list.span(),
                            "Empty list for #[zerogc(ignore_params)]"
                        ))
                    }
                    Meta::List(list) => list,
                    _ => return Err(Error::new(
                        meta.span(),
                        "Expected a list attribute for #[zerogc(ignore_params)]"
                    ))
                };
                for nested in list.nested {
                    match nested {
                        NestedMeta::Meta(Meta::Path(ref p))
                                if p.get_ident().is_some() => {
                            let ident = p.get_ident().unwrap();
                            if !result.ignore_params.insert(ident.clone()) {
                                return Err(Error::new(
                                    ident.span(),
                                    "Duplicate parameter to ignore"
                                ));
                            }
                        }
                        _ => return Err(Error::new(
                            nested.span(),
                            "Invalid list value for #[zerogc(ignore_param)]"
                        ))
                    }
                }
            } else if meta.path().is_ident("ignore_lifetimes") {
                if !result.ignore_params.is_empty() {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(ignore_lifetimes)]"
                    ))
                }
                let list = match meta {
                    Meta::List(ref list) if list.nested.is_empty() => {
                        return Err(Error::new(
                            list.span(),
                            "Empty list for #[zerogc(ignore_lifetimes)]"
                        ))
                    }
                    Meta::List(list) => list,
                    _ => return Err(Error::new(
                        meta.span(),
                        "Expected a list attribute for #[zerogc(ignore_lifetimes)]"
                    ))
                };
                for nested in list.nested {
                    let lifetime = match nested {
                        NestedMeta::Lit(Lit::Str(ref s)) => {
                            s.parse::<Lifetime>()?
                        },
                        NestedMeta::Meta(Meta::Path(ref p)) if p.get_ident().is_some() => {
                            let ident = p.get_ident().unwrap();
                            Lifetime {
                                ident: ident.clone(),
                                // Fake the appostrophie's span as matching the ident
                                apostrophe: ident.span()
                            }
                        },
                        _ => return Err(Error::new(
                            nested.span(),
                            "Invalid list value for #[zerogc(ignore_lifetimes)]"
                        ))
                    };
                    if !result.ignored_lifetimes.insert(lifetime.clone()) {
                        return Err(Error::new(
                            lifetime.span(),
                            "Duplicate lifetime to ignore"
                        ));
                    }
                }
            } else {
                return Err(Error::new(
                    meta.span(), "Unknown type flag"
                ))
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(result)
    }
}

#[proc_macro_derive(Trace, attributes(zerogc))]
pub fn derive_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let res = From::from(impl_derive_trace(&input)
        .unwrap_or_else(|e| e.to_compile_error()));
    debug_derive(
        "derive(Trace)",
        &format_args!("#[derive(Trace) for {}", input.ident),
        &res
    );
    res
}

fn impl_derive_trace(input: &DeriveInput) -> Result<TokenStream, syn::Error> {
    let info = GcTypeInfo::parse(input)?;
    let trace_impl = if info.config.nop_trace {
        impl_nop_trace(&input, &info)?
    } else {
        impl_trace(&input, &info)?
    };
    let brand_impl = impl_brand(&input, &info)?;
    let gc_safe_impl = impl_gc_safe(&input, &info)?;
    let extra_impls = impl_extras(&input, &info)?;
    Ok(quote! {
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
fn impl_extras(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let mut extra_items = Vec::new();
    let gc_lifetime = info.config.gc_lifetime();
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
                        #mutator_vis fn #mutator_name<Id>(self: ::zerogc::Gc<#gc_lifetime, Self, Id>, value: #value_ref_type)
                            where Id: ::zerogc::CollectorId,
                                   #value_ref_type: ::zerogc::GcDirectBarrier<#gc_lifetime, ::zerogc::Gc<#gc_lifetime, Self, Id>> {
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

fn impl_brand(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let mut generics: Generics = target.generics.clone();
    let mut rewritten_params = Vec::new();
    let mut rewritten_restrictions = Vec::new();
    for param in &mut generics.params {
        let rewritten_param: GenericArgument;
        match param {
            GenericParam::Type(ref mut type_param) => {
                let original_bounds = type_param.bounds.iter().cloned().collect::<Vec<_>>();
                type_param.bounds.push(parse_quote!(::zerogc::GcBrand<'new_gc, S>));
                let param_name = &type_param.ident;
                let rewritten_type: Type = parse_quote!(<#param_name as ::zerogc::GcBrand<'new_gc, S>>::Branded);
                rewritten_restrictions.push(WherePredicate::Type(PredicateType {
                    lifetimes: None,
                    bounded_ty: rewritten_type.clone(),
                    colon_token: Default::default(),
                    bounds: original_bounds.into_iter().collect()
                }));
                rewritten_param = GenericArgument::Type(rewritten_type);
            },
            GenericParam::Lifetime(ref l) => {
                if l.lifetime == info.config.gc_lifetime() {
                    rewritten_param = parse_quote!('new_gc);
                    assert!(!info.config.ignored_lifetimes.contains(&l.lifetime));
                } else if info.config.ignored_lifetimes.contains(&l.lifetime) {
                    rewritten_param = GenericArgument::Lifetime(l.lifetime.clone());
                } else {
                    return Err(Error::new(
                        l.span(),
                        "Unable to handle lifetime"
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
    impl_generics.params.push(GenericParam::Type(parse_quote!(S: ::zerogc::CollectorId)));
    impl_generics.make_where_clause().predicates.extend(rewritten_restrictions);
    let (_, ty_generics, _) = generics.split_for_impl();
    let (impl_generics, _, where_clause) = impl_generics.split_for_impl();
    Ok(quote! {
        unsafe impl #impl_generics ::zerogc::GcBrand<'new_gc, S>
            for #name #ty_generics #where_clause {
            type Branded = #name::<#(#rewritten_params),*>;
        }
    })
}
fn impl_trace(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let generics = add_trait_bounds_except(
        &target.generics, parse_quote!(zerogc::Trace),
        &info.config.ignore_params
    )?;
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
                        quote!(#ident)
                    }
                );
                let pattern = match variant.fields {
                    Fields::Named(ref fields) => {
                        let names = fields.named.iter()
                            .map(|field| field.ident.as_ref().unwrap());
                        quote!({ #(ref mut #names,)* })
                    },
                    Fields::Unnamed(ref fields) => {
                        let names = (0..fields.unnamed.len())
                            .map(|index| Ident::new(
                                &format!("field{}", index),
                                Span::call_site()
                            ));
                        quote!(( #(ref mut #names,)* ))
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
fn impl_gc_safe(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let generics = add_trait_bounds_except(
        &target.generics, parse_quote!(zerogc::GcSafe),
        &info.config.ignore_params
    )?;
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
    let does_need_drop = if info.config.is_copy {
        // If we're proven to be a copy type we don't need to be dropped
        quote!(false)
    } else {
        // We need to be dropped if any of our fields need to be dropped
        quote!(false #(|| <#field_types as ::zerogc::GcSafe>::NEEDS_DROP)*)
    };
    let fake_drop_impl = if info.config.is_copy {
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
    let verify_gc_safe = if info.config.is_copy {
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


fn impl_nop_trace(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let name = &target.ident;
    let generics = add_trait_bounds_except(
        &target.generics, parse_quote!(zerogc::Trace),
        &info.config.ignore_params
    )?;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let field_types: Vec<&Type>;
    match target.data {
        Data::Struct(ref data) => {
            field_types = data.fields.iter().map(|f| &f.ty).collect();
        },
        Data::Enum(ref data) => {
            field_types = data.variants.iter()
                .flat_map(|var| var.fields.iter().map(|f| &f.ty))
                .collect();
        },
        Data::Union(_) => {
            return Err(Error::new(
                name.span(),
                "Unions can't #[derive(Trace)]"
            ));
        },
    }
    let trace_assertions = field_types.iter()
        .map(|&t| {
            let ty_span = t.span();
            quote_spanned! { ty_span =>
                assert!(
                    !<#t as Trace>::NEEDS_TRACE,
                    "Can't #[derive(NullTrace) with {}",
                    stringify!(#t)
                );
            }
        }).collect::<Vec<_>>();
    Ok(quote! {
        unsafe impl #impl_generics ::zerogc::Trace for #name #ty_generics #where_clause {
            const NEEDS_TRACE: bool = false;

            #[inline] // Should be const-folded away
            fn visit<V: ::zerogc::GcVisitor>(&mut self, #[allow(unused)] visitor: &mut V) -> Result<(), V::Err> {
                #(#trace_assertions)*
                Ok(())
            }
        }
        unsafe impl #impl_generics ::zerogc::TraceImmutable for #name #ty_generics #where_clause {
            #[inline] // Should be const-folded away
            fn visit_immutable<V: ::zerogc::GcVisitor>(&self, #[allow(unused)] visitor: &mut V) -> Result<(), V::Err> {
                #(#trace_assertions)*
                Ok(())
            }
        }
        unsafe impl #impl_generics ::zerogc::NullTrace for #name #ty_generics #where_clause {}
    })
}

fn add_trait_bounds_except(
    generics: &Generics, bound: TypeParamBound,
    ignored_params: &HashSet<Ident>
) -> Result<Generics, Error> {
    let mut actually_ignored_args = HashSet::<Ident>::new();
    let generics = add_trait_bounds(
        &generics, bound,
        &mut |param: &TypeParam| {
            if ignored_params.contains(&param.ident) {
                actually_ignored_args.insert(param.ident.clone());
                true
            } else {
                false
            }
        }
    );
    if actually_ignored_args != *ignored_params {
        let missing = ignored_params - &actually_ignored_args;
        assert!(!missing.is_empty());
        let mut combined_error: Option<Error> = None;
        for missing in missing {
            let error = Error::new(
                missing.span(),
                "Unknown parameter",
            );
            match combined_error {
                Some(ref mut combined_error) => {
                    combined_error.combine(error);
                },
                None => {
                    combined_error = Some(error);
                }
            }
        }
        return Err(combined_error.unwrap());
    }
    Ok(generics)
}

fn add_trait_bounds(
    generics: &Generics, bound: TypeParamBound,
    should_ignore: &mut dyn FnMut(&TypeParam) -> bool
) -> Generics {
    let mut result: Generics = (*generics).clone();
    'paramLoop: for param in &mut result.params {
        if let GenericParam::Type(ref mut type_param) = *param {
            if should_ignore(type_param) {
                continue 'paramLoop;
            }
            type_param.bounds.push(bound.clone());
        }
    }
    result
}

fn debug_derive(key: &str, message: &dyn Display, value: &dyn Display) {
    // TODO: Use proc_macro::tracked_env::var
    match ::std::env::var_os("DEBUG_DERIVE") {
        Some(var) if var == "*" ||
            var.to_string_lossy().contains(key) => {
            // Enable this debug
        },
        _ => return,
    }
    eprintln!("{}:", message);
    use std::process::{Command, Stdio};
    let original_input = format!("{}", value);
    let cmd_res = Command::new("rustfmt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(original_input.as_bytes())?;
            drop(stdin);
            child.wait_with_output()
        });
    match cmd_res {
        Ok(output) if output.status.success() => {
            let formatted = String::from_utf8(output.stdout).unwrap();
            for line in formatted.lines() {
                eprintln!("  {}", line);
            }
        },
        // Fallthrough on failure
        Ok(output) => {
            eprintln!("Rustfmt error [code={}]:", output.status.code().map_or_else(
                || String::from("?"),
                |i| format!("{}", i)
            ));
            let err_msg = String::from_utf8(output.stderr).unwrap();
            for line in err_msg.lines() {
                eprintln!("  {}", line);
            }
            eprintln!("Original input: [[[[");
            for line in original_input.lines() {
                eprintln!("{}", line);
            }
            eprintln!("]]]]");
        }
        Err(e) => {
            eprintln!("Failed to run rustfmt: {}", e)
        }
    }
}
