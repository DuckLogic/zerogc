#![feature(
    proc_macro_tracked_env, // Used for `DEBUG_DERIVE`
    proc_macro_span, // Used for source file ids
)]
extern crate proc_macro;

use quote::{quote, quote_spanned};
use syn::{parse_macro_input, parenthesized, parse_quote, DeriveInput, Data, Error, Generics, GenericParam, TypeParamBound, Fields, Member, Index, Type, GenericArgument, Attribute, PathArguments, Meta, TypeParam, WherePredicate, PredicateType, Token, Lifetime, NestedMeta, Lit, Field};
use proc_macro2::{Ident, TokenStream, Span};
use syn::spanned::Spanned;
use syn::parse::{ParseStream, Parse};
use std::collections::HashSet;
use std::fmt::Display;
use std::io::Write;

mod macros;

/// Magic const that expands to either `::zerogc` or `crate::`
/// depending on whether we are currently bootstraping (compiling `zerogc` itself)
///
/// This is equivalant to `$crate` for regular macros
pub(crate) fn zerogc_crate() -> TokenStream {
    /*
     * TODO: A way to detect $crate
     * Checking environment variables
     * doesn't work well because doctests
     * and integration tests
     * will falsely believe they should access
     * `crate` instead of `zerogc`
     *
     * Instead, we re-export `extern crate self as zerogc`
     * at the start of the main crate
     */
    quote!(::zerogc)
}


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
    mutable: Option<MutableFieldOpts>,
    unsafe_skip_trace: bool
}
impl GcFieldAttrs {
    pub fn find(attrs: &[Attribute]) -> Result<Self, Error> {
        let mut iter = attrs.iter().filter_map(|attr| {
            if attr.path.is_ident("zerogc") {
                Some((attr, syn::parse2::<GcFieldAttrs>(attr.tokens.clone())))
            } else {
                None
            }
        });
        match (iter.next(), iter.next()) {
            (Some((_, parsed)), None) => {
                parsed
            },
            (Some(_), Some((dup_attr, _))) => {
                Err(Error::new(dup_attr.span(), "Duplicate #[zerogc] attr"))
            },
            (None, None) => Ok(GcFieldAttrs::default()),
            (None, Some(_)) => unreachable!()
        }
    }
}
impl Default for GcFieldAttrs {
    fn default() -> Self {
        GcFieldAttrs {
            mutable: None,
            unsafe_skip_trace: false
        }
    }
}
impl Parse for GcFieldAttrs {
    fn parse(raw_input: ParseStream) -> Result<Self, Error> {
        let input;
        parenthesized!(input in raw_input);
        let mut result = GcFieldAttrs::default();
        while !input.is_empty() {
            let flag_name = input.fork().parse::<Ident>()?;
            if flag_name == "mutable" {
                input.parse::<Ident>().unwrap(); // Actually advance
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
                continue
            }
            let meta = input.parse::<Meta>()?;
            if meta.path().is_ident("unsafe_skip_trace") {
                if !matches!(meta, Meta::Path(_)) {
                    return Err(Error::new(
                        meta.span(),
                        "Malformed attribute for #[zerogc(unsafe_skip_trace)]"
                    ))
                }
                if result.unsafe_skip_trace {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(unsafe_skip_trace)]"
                    ))
                }
                result.unsafe_skip_trace = true;
            } else {
                return Err(Error::new(
                    meta.path().span(),
                    "Unknown field flag"
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
    fn is_ignored_param(&self, param: &TypeParam) -> bool {
        self.config.ignore_params.contains(&param.ident) ||
            Some(&param.ident) == self.config.collector_id.as_ref()
    }
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
    collector_id: Option<Ident>,
    ignore_params: HashSet<Ident>,
    ignored_lifetimes: HashSet<Lifetime>,
    /// Unsafely assume the type is safe to [drop]
    /// from a GC, as consistent with the requirements of [GcSafe]
    ///
    /// This 'skips' the generation of a dummy drop
    unsafe_skip_drop: bool
}
impl TypeAttrs {
    fn gc_lifetime(&self) -> Lifetime {
        match self.gc_lifetime {
            Some(ref lt) => lt.clone(),
            None => Lifetime::new("'gc", Span::call_site())
        }
    }
    pub fn find(attrs: &[Attribute]) -> Result<Self, Error> {
        let mut iter = attrs.iter().filter_map(|attr| {
            if attr.path.is_ident("zerogc") {
                Some((attr, syn::parse2::<TypeAttrs>(attr.tokens.clone())))
            } else {
                None
            }
        });
        match (iter.next(), iter.next()) {
            (Some(_), Some((dup_attr, _))) => {
                return Err(Error::new(dup_attr.path.span(), "Duplicate #[zerogc] attribute"))
            },
            (Some((_, parsed)), None) => {
                parsed
            },
            (None, None) => Ok(TypeAttrs::default()),
            (None, Some(_)) => unreachable!()
        }
    }
}
impl Default for TypeAttrs {
    fn default() -> Self {
        TypeAttrs {
            is_copy: false,
            nop_trace: false,
            gc_lifetime: None,
            collector_id: None,
            ignore_params: Default::default(),
            ignored_lifetimes: Default::default(),
            unsafe_skip_drop: false
        }
    }
}
fn span_file_loc(span: Span) -> String {
    /*
     * Source file identifiers in the form `<file_name>:<lineno>`
     */
    let internal = span.unwrap();
    let sf = internal.source_file();
    let path = sf.path();
    let file_name = if sf.is_real() { path.file_name() } else { None }
        .map(std::ffi::OsStr::to_string_lossy)
        .map(String::from)
        .unwrap_or_else(|| String::from("<fake>"));
    let lineno = internal.start().line;
    format!("{}:{}", file_name, lineno)
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
            } else if meta.path().is_ident("unsafe_skip_drop") {
                if !matches!(meta, Meta::Path(_)) {
                    return Err(Error::new(
                        meta.span(),
                        "Malformed attribute for #[zerogc(unsafe_skip_drop)]"
                    ))
                }
                if result.unsafe_skip_drop {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(unsafe_skip_drop)]"
                    ))
                }
                result.unsafe_skip_drop = true;
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
            } else if meta.path().is_ident("collector_id") {
                if result.collector_id.is_some() {
                    return Err(Error::new(
                        meta.span(),
                        "Duplicate flags: #[zerogc(collector_id)]"
                    ))
                }
                fn get_ident_meta(meta: &NestedMeta) -> Option<&Ident> {
                    match *meta {
                        NestedMeta::Meta(Meta::Path(ref p)) => p.get_ident(),
                        _ => None
                    }
                }
                let ident = match meta {
                    Meta::List(ref l) if l.nested.len() == 1
                        && get_ident_meta(&l.nested[0]).is_some() => {
                        get_ident_meta(&l.nested[0]).unwrap()
                    }
                    _ => {
                        return Err(Error::new(
                            meta.span(),
                            "Malformed attribute for #[zerogc(collector_id)]"
                        ))
                    }
                };
                result.collector_id = Some(ident.clone());
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
                if !result.ignored_lifetimes.is_empty() {
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

#[proc_macro]
pub fn unsafe_gc_impl(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let parsed = parse_macro_input!(input as macros::MacroInput);
    let res = parsed.expand_output()
        .unwrap_or_else(|e| e.to_compile_error());
    let span_loc = span_file_loc(Span::call_site());
    debug_derive(
        "unsafe_gc_impl!",
        &span_loc,
        &format_args!("unsafe_gc_impl! @ {}", span_loc),
        &res
    );
    res.into()
}

#[proc_macro_derive(NullTrace, attributes(zerogc))]
pub fn derive_null_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let mut info = match GcTypeInfo::parse(&input) {
        Ok(info) => info,
        Err(e) => return e.to_compile_error().into()
    };
    if info.config.nop_trace {
        return quote_spanned! { input.ident.span() =>
            compile_error!("derive(NullTrace): Can't explicitly specify #[zerogc(nop_trace)] as it's already implied")
        }.into();
    }
    info.config.nop_trace = true;
    let res = From::from(impl_derive_trace(&input, &info)
        .unwrap_or_else(|e| e.to_compile_error()));
    debug_derive(
        "derive(NullTrace)",
        &input.ident.to_string(),
        &format_args!("#[derive(NullTrace) for {}", input.ident),
        &res
    );
    res
}

#[proc_macro_derive(Trace, attributes(zerogc))]
pub fn derive_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let info = match GcTypeInfo::parse(&input) {
        Ok(info) => info,
        Err(e) => return e.to_compile_error().into()
    };
    let res = From::from(impl_derive_trace(&input, &info)
        .unwrap_or_else(|e| e.to_compile_error()));
    debug_derive(
        "derive(Trace)",
        &input.ident.to_string(),
        &format_args!("#[derive(Trace) for {}", input.ident),
        &res
    );
    res
}

fn impl_derive_trace(input: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, syn::Error> {
    let trace_impl = if info.config.nop_trace {
        impl_nop_trace(&input, &info)?
    } else {
        impl_trace(&input, &info)?
    };
    let rebrand_impl = if info.config.nop_trace {
        impl_rebrand_nop(&input, &info)?
    } else {
        impl_rebrand(&input, &info)?
    };
    let erase_impl = if info.config.nop_trace {
        impl_erase_nop(&input, &info)?
    } else {
        impl_erase(&input, &info)?
    };
    let gc_safe_impl = impl_gc_safe(&input, &info)?;
    let extra_impls = impl_extras(&input, &info)?;
    Ok(quote! {
        #trace_impl
        #rebrand_impl
        #erase_impl
        #gc_safe_impl
        #extra_impls
    })
}

fn trace_fields(fields: &Fields, access_ref: &mut dyn FnMut(Member) -> TokenStream) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    // TODO: Detect if we're a unit struct and implement `NullTrace`
    let mut result = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let val = access_ref(match field.ident {
            Some(ref ident) => Member::Named(ident.clone()),
            None => Member::Unnamed(Index::from(index))
        });
        let attrs = GcFieldAttrs::find(&field.attrs)?;
        if attrs.unsafe_skip_trace {
            continue // Skip
        }
        result.push(quote!(#zerogc_crate::Trace::visit(#val, &mut *visitor)?));
    }
    Ok(quote!(#(#result;)*))
}

/// Implement extra methods
///
/// 1. Implement setters for `GcCell` fields using a write barrier
fn impl_extras(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
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
                    let field_as_ptr = quote_spanned!(field.span() => #zerogc_crate::cell::GcCell::as_ptr(&(*self.value()).#original_name));
                    let barrier = quote_spanned!(field.span() => #zerogc_crate::GcDirectBarrier::write_barrier(&value, &self, offset));
                    let id_name = info.config.collector_id.as_ref().cloned()
                        .unwrap_or_else(|| parse_quote!(Id));
                    let mut id_generics: Generics = if let Some(ref id_name) = info.config.collector_id {
                        let mut generics = Generics::default();
                        generics.make_where_clause().predicates.push(parse_quote!(#id_name: CollectorId));
                        generics
                    } else {
                        parse_quote!(<Id: CollectorId>)
                    };
                    id_generics.make_where_clause().predicates.push(parse_quote!(
                        #value_ref_type: #zerogc_crate::GcDirectBarrier<
                            #gc_lifetime,
                            #zerogc_crate::Gc<#gc_lifetime, Self, #id_name>
                        >
                    ));
                    let where_clause = &id_generics.where_clause;
                    extra_items.push(quote! {
                        #[inline] // TODO: Implement `GcDirectBarrier` ourselves
                        #mutator_vis fn #mutator_name #id_generics(self: #zerogc_crate::Gc<#gc_lifetime, Self, #id_name>, value: #value_ref_type)
                            #where_clause {
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


fn impl_erase_nop(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    let name = &target.ident;
    let mut generics: Generics = target.generics.clone();
    for param in &mut generics.params {
        match param {
            GenericParam::Type(ref mut type_param) => {
                // Require all params are NullTrace
                type_param.bounds.push(parse_quote!(#zerogc_crate::NullTrace));
            },
            GenericParam::Lifetime(ref mut l) => {
                if l.lifetime == info.config.gc_lifetime() {
                    assert!(!info.config.ignored_lifetimes.contains(&l.lifetime));
                    return Err(Error::new(
                        l.lifetime.span(),
                        "Unexpected GC lifetime: Expected #[zerogc(nop_trace)] during #[derive(GcErase)]"
                    ))
                } else if info.config.ignored_lifetimes.contains(&l.lifetime) {
                    // Explicitly ignored is okay, as long as it outlives the `'min`
                    l.bounds.push(parse_quote!('min));
                } else {
                    return Err(Error::new(
                        l.span(),
                        "Lifetime must be explicitly ignored"
                    ))
                }
            },
            GenericParam::Const(_) => {}
        }
    }
    let mut impl_generics = generics.clone();
    impl_generics.params.push(GenericParam::Lifetime(parse_quote!('min)));
    let collector_id = match info.config.collector_id {
        Some(ref id) => id.clone(),
        None => {
            impl_generics.params.push(GenericParam::Type(parse_quote!(Id: #zerogc_crate::CollectorId)));
            parse_quote!(Id)
        }
    };
    // Require that `Self: NullTrace`
    impl_generics.make_where_clause().predicates.push(WherePredicate::Type(PredicateType {
        lifetimes: None,
        bounded_ty: parse_quote!(Self),
        bounds: parse_quote!(#zerogc_crate::NullTrace),
        colon_token: Default::default()
    }));
    let (_, ty_generics, _) = generics.split_for_impl();
    let (impl_generics, _, where_clause) = impl_generics.split_for_impl();
    Ok(quote! {
        unsafe impl #impl_generics #zerogc_crate::GcErase<'min, #collector_id>
            for #name #ty_generics #where_clause {
            // We can pass-through because we are NullTrace
            type Erased = Self;
        }
    })
}
fn impl_erase(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    let name = &target.ident;
    let mut generics: Generics = target.generics.clone();
    let mut rewritten_params = Vec::new();
    let mut rewritten_restrictions = Vec::new();
    let collector_id = match info.config.collector_id {
        Some(ref id) => id.clone(),
        None => parse_quote!(Id)
    };
    for param in &mut generics.params {
        let rewritten_param: GenericArgument;
        fn unsupported_lifetime_param(lt: &Lifetime) -> Error {
            Error::new(
                lt.span(),
                "Unless Self: NullTrace, derive(GcErase) is currently unable to handle lifetimes"
            )
        }
        match param {
            GenericParam::Type(ref mut type_param) => {
                let param_name = &type_param.ident;
                if info.is_ignored_param(&*type_param) {
                    let param_name = &type_param.ident;
                    rewritten_params.push(parse_quote!(#param_name));
                    continue
                }
                fn rewrite_bound(bound: &TypeParamBound, info: &GcTypeInfo) -> Result<TypeParamBound, Error> {
                    match bound {
                        TypeParamBound::Trait(ref t) => Ok(TypeParamBound::Trait(t.clone())),
                        TypeParamBound::Lifetime(ref lt) if *lt == info.config.gc_lifetime() => {
                            Ok(parse_quote!('new_gc))
                        },
                        TypeParamBound::Lifetime(ref lt) => {
                            return Err(unsupported_lifetime_param(lt))
                        }
                    }
                }
                let original_bounds = type_param.bounds.iter()
                    .map(|bound| rewrite_bound(bound, info))
                    .collect::<Result<Vec<_>, _>>()?;
                type_param.bounds.push(parse_quote!(#zerogc_crate::GcErase<'min, #collector_id>));
                type_param.bounds.push(parse_quote!('min));
                let rewritten_type: Type = parse_quote!(<#param_name as #zerogc_crate::GcErase<'min, #collector_id>>::Erased);
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
                    rewritten_param = parse_quote!('min);
                    assert!(!info.config.ignored_lifetimes.contains(&l.lifetime));
                } else {
                    return Err(unsupported_lifetime_param(&l.lifetime))
                }
            },
            GenericParam::Const(ref param) => {
                let name = &param.ident;
                rewritten_param = GenericArgument::Const(parse_quote!(#name));
            }
        }
        rewritten_params.push(rewritten_param);
    }
    let mut field_types = Vec::new();
    match target.data {
        Data::Struct(ref s) => {
            for f in &s.fields {
                field_types.push(f.ty.clone());
            }
        },
        Data::Enum(ref e) => {
            for variant in &e.variants {
                for f in &variant.fields {
                    field_types.push(f.ty.clone());
                }
            }
        },
        Data::Union(_) => {
            return Err(Error::new(target.ident.span(), "Unable to derive(GcErase) for unions"))
        }
    }
    let mut impl_generics = generics.clone();
    impl_generics.params.push(GenericParam::Lifetime(parse_quote!('min)));
    if info.config.collector_id.is_none() {
        impl_generics.params.push(GenericParam::Type(parse_quote!(Id: #zerogc_crate::CollectorId)));
    }
    impl_generics.make_where_clause().predicates.extend(rewritten_restrictions);
    let (_, ty_generics, _) = generics.split_for_impl();
    let (impl_generics, _, where_clause) = impl_generics.split_for_impl();
    let assert_erase = field_types.iter().map(|field_type| {
        let span = field_type.span();
        quote_spanned!(span => <#field_type as #zerogc_crate::GcErase<'min, #collector_id>>::assert_erase();)
    }).collect::<Vec<_>>();
    Ok(quote! {
        unsafe impl #impl_generics #zerogc_crate::GcErase<'min, #collector_id>
            for #name #ty_generics #where_clause {
            type Erased = #name::<#(#rewritten_params),*>;

            fn assert_erase() {
                #(#assert_erase)*
            }
        }
    })
}


fn impl_rebrand_nop(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    let name = &target.ident;
    let mut generics: Generics = target.generics.clone();
    for param in &mut generics.params {
        match param {
            GenericParam::Type(ref mut type_param) => {
                // Require all params are NullTrace
                type_param.bounds.push(parse_quote!(#zerogc_crate::NullTrace));
            },
            GenericParam::Lifetime(ref mut l) => {
                if l.lifetime == info.config.gc_lifetime() {
                    assert!(!info.config.ignored_lifetimes.contains(&l.lifetime));
                    return Err(Error::new(
                        l.lifetime.span(),
                        "Unexpected GC lifetime: Expected #[zerogc(nop_trace)] during #[derive(GcRebrand)]"
                    ))
                } else if info.config.ignored_lifetimes.contains(&l.lifetime) {
                    // Explicitly ignored is okay, as long as it outlives the `'new_gc`
                    l.bounds.push(parse_quote!('new_gc));
                } else {
                    return Err(Error::new(
                        l.span(),
                        "Lifetime must be explicitly ignored"
                    ))
                }
            },
            GenericParam::Const(_) => {}
        }
    }
    let mut impl_generics = generics.clone();
    impl_generics.params.push(GenericParam::Lifetime(parse_quote!('new_gc)));
    let collector_id = match info.config.collector_id {
        Some(ref id) => id.clone(),
        None => {
            impl_generics.params.push(GenericParam::Type(parse_quote!(Id: #zerogc_crate::CollectorId)));
            parse_quote!(Id)
        }
    };
    // Require that `Self: NullTrace`
    impl_generics.make_where_clause().predicates.push(WherePredicate::Type(PredicateType {
        lifetimes: None,
        bounded_ty: parse_quote!(Self),
        bounds: parse_quote!(#zerogc_crate::NullTrace),
        colon_token: Default::default()
    }));
    let (_, ty_generics, _) = generics.split_for_impl();
    let (impl_generics, _, where_clause) = impl_generics.split_for_impl();
    Ok(quote! {
        unsafe impl #impl_generics #zerogc_crate::GcRebrand<'new_gc, #collector_id>
            for #name #ty_generics #where_clause {
            // We can pass-through because we are NullTrace
            type Branded = Self;
        }
    })
}
fn impl_rebrand(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    let name = &target.ident;
    let mut generics: Generics = target.generics.clone();
    let mut rewritten_params = Vec::new();
    let mut rewritten_restrictions = Vec::new();
    let collector_id = match info.config.collector_id {
        Some(ref id) => id.clone(),
        None => parse_quote!(Id)
    };
    for param in &mut generics.params {
        let rewritten_param: GenericArgument;
        fn unsupported_lifetime_param(lt: &Lifetime) -> Error {
            Error::new(
                lt.span(),
                "Unless Self: NullTrace, derive(GcRebrand) is currently unable to handle lifetimes"
            )
        }
        match param {
            GenericParam::Type(ref mut type_param) => {
                let param_name = &type_param.ident;
                if info.is_ignored_param(&*type_param) {
                    rewritten_params.push(parse_quote!(#param_name));
                    continue
                }
                let original_bounds = type_param.bounds.iter()
                    .map(|bound| rewrite_bound(bound, info))
                    .collect::<Result<Vec<_>, Error>>()?;
                fn rewrite_bound(bound: &TypeParamBound, info: &GcTypeInfo) -> Result<TypeParamBound, Error> {
                    match bound {
                        TypeParamBound::Trait(ref t) => Ok(TypeParamBound::Trait(t.clone())),
                        TypeParamBound::Lifetime(ref lt) if *lt == info.config.gc_lifetime() => {
                            Ok(parse_quote!('new_gc))
                        },
                        TypeParamBound::Lifetime(ref lt) => {
                            return Err(unsupported_lifetime_param(lt))
                        }
                    }
                }
                type_param.bounds.push(parse_quote!(#zerogc_crate::GcRebrand<'new_gc, #collector_id>));
                let rewritten_type: Type = parse_quote!(<#param_name as #zerogc_crate::GcRebrand<'new_gc, #collector_id>>::Branded);
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
                } else {
                    return Err(unsupported_lifetime_param(&l.lifetime));
                }
            },
            GenericParam::Const(ref param) => {
                let name = &param.ident;
                rewritten_param = GenericArgument::Const(parse_quote!(#name));
            }
        }
        rewritten_params.push(rewritten_param);
    }
    let mut field_types = Vec::new();
    match target.data {
        Data::Struct(ref s) => {
            for f in &s.fields {
                field_types.push(f.ty.clone());
            }
        },
        Data::Enum(ref e) => {
            for variant in &e.variants {
                for f in &variant.fields {
                    field_types.push(f.ty.clone());
                }
            }
        },
        Data::Union(_) => {
            return Err(Error::new(target.ident.span(), "Unable to derive(GcErase) for unions"))
        }
    }
    let mut impl_generics = generics.clone();
    impl_generics.params.push(GenericParam::Lifetime(parse_quote!('new_gc)));
    if info.config.collector_id.is_none() {
        impl_generics.params.push(GenericParam::Type(parse_quote!(Id: #zerogc_crate::CollectorId)));
    }
    let assert_rebrand = field_types.iter().map(|field_type| {
        let span = field_type.span();
        quote_spanned!(span => <#field_type as #zerogc_crate::GcRebrand<'new_gc, #collector_id>>::assert_rebrand();)
    }).collect::<Vec<_>>();
    impl_generics.make_where_clause().predicates.extend(rewritten_restrictions);
    let (_, ty_generics, _) = generics.split_for_impl();
    let (impl_generics, _, where_clause) = impl_generics.split_for_impl();
    Ok(quote! {
        unsafe impl #impl_generics #zerogc_crate::GcRebrand<'new_gc, #collector_id>
            for #name #ty_generics #where_clause {
            type Branded = #name::<#(#rewritten_params),*>;

            fn assert_rebrand() {
                #(#assert_rebrand)*
            }
        }
    })
}
fn impl_trace(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    let name = &target.ident;
    let generics = add_trait_bounds_except(
        &target.generics, parse_quote!(#zerogc_crate::Trace),
        &info.config.ignore_params, None
    )?;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let trace_impl: TokenStream;
    match target.data {
        Data::Struct(ref data) => {
            trace_impl = trace_fields(
                &data.fields,
                &mut |member| quote!(&mut self.#member)
            )?;
        },
        Data::Enum(ref data) => {
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
                )?;
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
    let fields = collect_fields(target)?;
    let does_need_drop = needs_drop(info, &*fields);
    let fields_need_trace = fields.iter()
        .filter_map(|(field, attrs)| {
            if attrs.unsafe_skip_trace {
                return None
            }
            let field_type = &field.ty;
            Some(quote_spanned!(field_type.span() => <#field_type as #zerogc_crate::Trace>::NEEDS_TRACE))
        });
    Ok(quote! {
        unsafe impl #impl_generics #zerogc_crate::Trace for #name #ty_generics #where_clause {
            const NEEDS_TRACE: bool = false #(|| #fields_need_trace)*;
            const NEEDS_DROP: bool = #does_need_drop;

            /*
             * The inline annotation adds this function's MIR to the metadata.
             * Without it cross-crate inlining is impossible (without LTO).
             */
            #[inline]
            fn visit<Visitor: #zerogc_crate::GcVisitor + ?Sized>(&mut self, #[allow(unused)] visitor: &mut Visitor) -> Result<(), Visitor::Err> {
                #trace_impl
                Ok(())
            }
            #[inline]
            unsafe fn visit_inside_gc<'actual_gc, Visitor, ActualId>(gc: &mut #zerogc_crate::Gc<'actual_gc, Self, ActualId>, visitor: &mut Visitor) -> Result<(), Visitor::Err>
                where Visitor: #zerogc_crate::GcVisitor, ActualId: #zerogc_crate::CollectorId, Self: #zerogc_crate::GcSafe + 'actual_gc {
                // Assume we are Sized
                visitor.visit_gc(gc)
            }
        }
    })
}
fn collect_fields(target: &DeriveInput) -> Result<Vec<(&Field, GcFieldAttrs)>, Error> {
    let target_fields: Vec<&Field> = match target.data {
        Data::Struct(ref data) => {
            data.fields.iter().collect()
        },
        Data::Enum(ref data) => {
            data.variants.iter()
                .flat_map(|v| v.fields.iter())
                .collect()
        },
        Data::Union(_) => {
            return Err(Error::new(
                target.ident.span(), "Unions are unsupported"
            ))
        },
    };
    target_fields.into_iter()
        .map(|field| Ok((field, GcFieldAttrs::find(&field.attrs)?)))
        .collect::<Result<Vec<_>, Error>>()
}
fn impl_gc_safe(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    let name = &target.ident;
    let collector_id = &info.config.collector_id;
    let generics = add_trait_bounds_except(
        &target.generics, parse_quote!(#zerogc_crate::GcSafe),
        &info.config.ignore_params,
        Some(&mut |other: &Ident| {
            if let Some(ref collector_id) = *collector_id {
                other == collector_id // -> ignore collector_id for GcSafe
            } else {
                false
            }
        })
    )?;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let target_fields = collect_fields(target)?;
    let fake_drop_impl = if info.config.is_copy {
        /*
         * If this type can be proven to implement Copy,
         * it is known to have no destructor.
         * We don't need a fake drop to prevent misbehavior.
         * In fact, adding one would prevent it from being Copy
         * in the first place.
         */
        quote!()
    } else if info.config.unsafe_skip_drop {
        quote!() // Skip generating drop at user's request
    } else if info.config.nop_trace {
        /*
         * A NullTrace type is implicitly drop safe.
         *
         * No tracing needed implies there are no reachable GC references.
         * Therefore there is nothing to fear about finalizers resurrecting things
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
        quote!(#zerogc_crate::assert_copy::<Self>())
    } else {
        let field_assertions = target_fields.iter().filter_map(|&(field, ref attrs)| {
            if attrs.unsafe_skip_trace { return None }
            let span = field.ty.span();
            let field_type = &field.ty;
            Some(quote_spanned!(span => <#field_type as #zerogc_crate::GcSafe>::assert_gc_safe()))
        });
        quote!(#(#field_assertions;)*)
    };
    Ok(quote! {
        unsafe impl #impl_generics #zerogc_crate::GcSafe
            for #name #ty_generics #where_clause {
            fn assert_gc_safe() {
                #verify_gc_safe
            }
        }
        #fake_drop_impl
    })
}

fn needs_drop(info: &GcTypeInfo, target_fields: &[(&Field, GcFieldAttrs)]) -> TokenStream {
    let zerogc_crate = zerogc_crate();
    if info.config.is_copy {
        // If we're proven to be a copy type we don't need to be dropped
        quote!(false)
    } else if info.config.unsafe_skip_drop {
        // Skipping the auto-generated Drop impl means its fine to use `mem::needs_drop`
        quote!(core::mem::needs_drop::<Self>())
    } else {
        let drop_assertions = target_fields.iter().filter_map(|(field, attrs)| {
            if attrs.unsafe_skip_trace { return None }
            let span = field.ty.span();
            let field_type = &field.ty;
            Some(quote_spanned!(span => <#field_type as #zerogc_crate::Trace>::NEEDS_DROP))
        });
        // We need to be dropped if any of our fields need to be dropped
        quote!(false #(|| #drop_assertions)*)
    }
}

fn impl_nop_trace(target: &DeriveInput, info: &GcTypeInfo) -> Result<TokenStream, Error> {
    let zerogc_crate = zerogc_crate();
    let name = &target.ident;
    let generics = add_trait_bounds_except(
        &target.generics, parse_quote!(#zerogc_crate::Trace),
        &info.config.ignore_params, None
    )?;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let target_fields = collect_fields(target)?;
    // TODO: We should have some sort of const-assertion for this....
    let trace_assertions = target_fields.iter()
        .map(|&(field, ref field_attrs)| {
            let t = &field.ty;
            if field_attrs.unsafe_skip_trace {
                return Ok(quote!());
            }
            let ty_span = t.span();
            Ok(quote_spanned! { ty_span =>
                assert!(
                    !<#t as #zerogc_crate::Trace>::NEEDS_TRACE,
                    "Can't #[derive(NullTrace) with {}",
                    stringify!(#t)
                );
            })
        }).collect::<Result<Vec<_>, Error>>()?;
    let needs_drop = needs_drop(info, &*target_fields);
    Ok(quote! {
        unsafe impl #impl_generics #zerogc_crate::Trace for #name #ty_generics #where_clause {
            const NEEDS_TRACE: bool = false;
            const NEEDS_DROP: bool = #needs_drop;

            #[inline] // Should be const-folded away
            fn visit<Visitor: #zerogc_crate::GcVisitor + ?Sized>(&mut self, #[allow(unused)] visitor: &mut Visitor) -> Result<(), Visitor::Err> {
                #(#trace_assertions)*
                Ok(())
            }
            #[inline]
            unsafe fn visit_inside_gc<'actual_gc, Visitor, ActualId>(gc: &mut #zerogc_crate::Gc<'actual_gc, Self, ActualId>, visitor: &mut Visitor) -> Result<(), Visitor::Err>
                where Visitor: #zerogc_crate::GcVisitor, ActualId: #zerogc_crate::CollectorId, Self: #zerogc_crate::GcSafe + 'actual_gc {
                // Assume we are Sized
                visitor.visit_gc(gc)
            }
        }
        unsafe impl #impl_generics #zerogc_crate::TraceImmutable for #name #ty_generics #where_clause {
            #[inline] // Should be const-folded away
            fn visit_immutable<Visitor: #zerogc_crate::GcVisitor + ?Sized>(&self, #[allow(unused)] visitor: &mut Visitor) -> Result<(), Visitor::Err> {
                #(#trace_assertions)*
                Ok(())
            }
        }
        unsafe impl #impl_generics #zerogc_crate::NullTrace for #name #ty_generics #where_clause {}
    })
}

fn add_trait_bounds_except(
    generics: &Generics, bound: TypeParamBound,
    ignored_params: &HashSet<Ident>,
    mut extra_ignore: Option<&mut dyn FnMut(&Ident) -> bool>
) -> Result<Generics, Error> {
    let mut actually_ignored_args = HashSet::<Ident>::new();
    let generics = add_trait_bounds(
        &generics, bound,
        &mut |param: &TypeParam| {
            if let Some(ref mut extra) = extra_ignore {
                if extra(&param.ident) {
                    return true; // ignore (but don't add to set)
                }
            }
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

fn debug_derive(key: &str, target: &dyn ToString, message: &dyn Display, value: &dyn Display) {
    let target = target.to_string();
    // TODO: Use proc_macro::tracked_env::var
    match ::proc_macro::tracked_env::var("DEBUG_DERIVE") {
        Ok(ref var) if var == "*" || var == "1" || var.is_empty() => {}
        Ok(ref var) if var == "0" => { return /* disabled */ }
        Ok(var) => {
            let target_parts = std::iter::once(key)
                .chain(target.split(":")).collect::<Vec<_>>();
            for pattern in var.split_terminator(",") {
                let pattern_parts = pattern.split(":").collect::<Vec<_>>();
                if pattern_parts.len() > target_parts.len() { continue }
                for (&pattern_part, &target_part) in pattern_parts.iter()
                    .chain(std::iter::repeat(&"*")).zip(&target_parts) {
                    if pattern_part == "*" {
                        continue // Wildcard matches anything: Keep checking
                    }
                    if pattern_part != target_part {
                        return // Pattern mismatch
                    }
                }
            }
            // Fallthrough -> enable this debug
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
