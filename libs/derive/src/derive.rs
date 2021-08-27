use darling::{Error, FromMeta, FromGenerics, FromTypeParam, FromDeriveInput, FromVariant, FromField};
use proc_macro2::{Ident, TokenStream, Span};
use syn::{Generics, Type, LifetimeDef, GenericParam, TypeParam, Token, Lifetime, Path, parse_quote, PathArguments, GenericArgument, TypePath};
use syn::punctuated::Punctuated;
use darling::util::{SpannedValue, WithOriginal};
use quote::{quote_spanned, quote, format_ident, ToTokens};
use darling::ast::{Style, Data};
use std::collections::HashSet;
use syn::spanned::Spanned;

#[derive(Copy, Clone, Debug)]
pub enum TraceDeriveKind {
    NullTrace,
    Regular
}

#[derive(Debug, FromMeta)]
struct LifetimeAttrs {
    #[darling(default)]
    ignore: bool,
    #[darling(default)]
    gc_lifetime: bool
}
#[derive(Debug, Default)]
struct TraceGenerics {
    gc_lifetime: Option<Lifetime>,
    ignored_lifetimes: Vec<Lifetime>,
    type_params: Vec<TraceTypeParam>
}
impl TraceGenerics {
    /// Return all the "regular" type parameters,
    /// excluding `Id` parameters and  those marked `#[zerogc(ignore)]`
    fn regular_type_params(&self) -> impl Iterator<Item=&'_ TraceTypeParam> + '_ {
        self.type_params.iter().filter(|param| !param.ignore && !param.is_collector_id())
    }
    fn normalize(&mut self) -> Result<(), Error> {
        for tp in &mut self.type_params {
            tp.normalize()?;
        }
        Ok(())
    }
    fn _handle_type(&mut self, tp: &TypeParam) -> Result<(), Error> {
        self.type_params.push(TraceTypeParam::from_type_param(tp)?);
        Ok(())
    }
    fn _handle_lifetime(&mut self, lt: &LifetimeDef) -> Result<(), Error> {
        let opts = lt.attrs.iter()
            .find(|attr| attr.path.is_ident("zerogc"))
            .map(|attr| attr.parse_meta().map_err(Into::into).and_then(|meta| LifetimeAttrs::from_meta(&meta)))
            .transpose()?
            .unwrap_or_else(|| LifetimeAttrs {
                gc_lifetime: lt.lifetime.ident == "gc",
                ignore: false
            });
        match (opts.ignore, opts.gc_lifetime) {
            (true, true) => {
                return Err(Error::custom("Attribute can't be a gc_lifetime but also be ignored").with_span(lt))
            },
            (true, false) => {
                self.ignored_lifetimes.push(lt.lifetime.clone());
            },
            (false, true) => {
                if self.gc_lifetime.is_some() {
                    return Err(Error::custom("Multiple gc lifetimes (try specifying #[zerogc(gc_lifetime = false)] to suppress implicit handling of 'gc attribute)"))
                }
                self.gc_lifetime = Some(lt.lifetime.clone());
            },
            (false, false) => {
                return Err(Error::custom("Attribute must be either ignored or 'gc").with_span(lt))
            }
        }
        Ok(())
    }
}
impl FromGenerics for TraceGenerics {
    fn from_generics(generics: &Generics) -> Result<Self, Error> {
        let mut res = Self::default();
        let mut errors = Vec::new();
        for param in generics.params.iter() {
            match *param {
                GenericParam::Type(ref tp) => {
                    match res._handle_type(tp) {
                        Ok(()) => {},
                        Err(e) => {
                            errors.push(e);
                        }
                    }
                }
                GenericParam::Lifetime(ref lt) => {
                    match res._handle_lifetime(lt) {
                        Ok(()) => {},
                        Err(e) => {
                            errors.push(e);
                        }
                    }
                }
                GenericParam::Const(_) => {
                    errors.push(Error::custom("NYI: const parameters are currently unsupported")
                        .with_span(param))
                }
            }
        }
        if errors.is_empty() {
            Ok(res)
        } else {
            Err(Error::multiple(errors))
        }
    }
}

#[derive(Debug, FromTypeParam)]
#[darling(attributes(zerogc))]
pub struct TraceTypeParam {
    ident: Ident,
    bounds: Vec<syn::TypeParamBound>,
    #[darling(default)]
    ignore: bool,
    /// Mark this type as a `CollectorId`
    #[darling(default)]
    collector_id: Option<bool>,
}
impl TraceTypeParam {
    fn is_collector_id(&self) -> bool {
        matches!(self.collector_id, Some(true))
    }
    fn normalize(&mut self) -> Result<(), Error> {
        if self.ignore && self.is_collector_id() {
            return Err(Error::custom("Type parameter can't be ignored but also collector_id"))
        }
        if self.ident == "Id" && self.collector_id.is_none() {
            crate::emit_warning(
                "Type parameter is named `Id` but isn't marked #[zerogc(collector_id)]. Specify collector_id = false if this is intentional.",
                self.ident.span()
            )
        }
        Ok(())
    }
}

#[derive(Debug, FromMeta)]
struct MutableFieldOptions {
    #[darling(default)]
    public: bool,
}

#[derive(Debug, FromField)]
struct TraceField {
    ident: Option<Ident>,
    ty: Type,
    #[darling(default)]
    mutable: Option<MutableFieldOptions>,
    /// Unsafely skip tracing the field.
    ///
    /// Undefined behavior if the type actually needs
    /// to be traced.
    #[darling(default)]
    unsafe_skip_trace: bool,
}
impl TraceField {
    fn expand_trace(&self, idx: usize, access: &FieldAccess) -> TokenStream {
        let access = match self.ident {
            Some(ref name) => access.access_named_field(name.clone()),
            None => access.access_indexed_field(idx, self.ty.span())
        };
        let ty = &self.ty;
        quote_spanned!(self.ty.span() => <#ty as zerogc::Trace>::visit(&#access, gc_visitor))
    }
}

#[derive(Debug, FromVariant)]
struct TraceVariant {
    ident: Ident,
    fields: darling::ast::Fields<TraceField>
}
impl TraceVariant {
    fn fields(&self) -> impl Iterator<Item=&'_ TraceField> + '_ {
        self.fields.iter()
    }
    pub fn destructure(&self) -> (TokenStream, FieldAccess) {
        match self.fields.style {
            Style::Unit => (quote!(), FieldAccess::None),
            Style::Tuple => {
                let prefix: Ident = parse_quote!(member);
                let destructure = self.fields.iter().enumerate().map(|(idx, field)| {
                    let name = format_ident!("member{}", span = field.ty.span(), idx);
                    quote!(ref #name)
                });
                (quote!((#(#destructure),*)), FieldAccess::Variable {
                    prefix: Some(prefix)
                })
            },
            Style::Struct => {
                let fields = self.fields.iter()
                    .map(|field| field.ident.as_ref().unwrap());
                (quote!({ #(ref #fields),* }), FieldAccess::SelfMember)
            }
        }
    }
}

#[derive(FromDeriveInput)]
#[darling(attributes(zerogc))]
pub struct TraceDeriveInput {
    pub ident: Ident,
    generics: WithOriginal<TraceGenerics, syn::Generics>,
    data: darling::ast::Data<TraceVariant, TraceField>,
    // TODO: Eventually remove this option
    #[darling(default)]
    nop_trace: SpannedValue<bool>,
    #[darling(default)]
    collector_ids: Option<Punctuated<Path, Token![,]>>,
    /// Unsafely assume the type is safe to [drop]
    /// from a GC, as consistent with the requirements of [GcSafe]
    ///
    /// This 'skips' the generation of a dummy drop
    #[darling(default)]
    unsafe_skip_drop: bool,
    #[darling(default, rename = "copy")]
    is_copy: bool,
}
impl TraceDeriveInput {
    pub fn determine_field_types(&self) -> HashSet<Type> {
        match self.data {
            Data::Enum(ref variants) => {
                variants.iter()
                    .flat_map(|var| var.fields())
                    .map(|fd| &fd.ty)
                    .cloned()
                    .collect()
            }
            Data::Struct(ref s) => {
                s.fields.iter().map(|f| &f.ty).cloned().collect()
            }
        }
    }
    pub fn normalize(&mut self, kind: TraceDeriveKind) -> Result<(), Error> {
        if *self.nop_trace {
            crate::emit_warning("#[zerogc(nop_trace)] is deprecated (use #[derive(NullTrace)] instead)", self.nop_trace.span())
        }
        if let Some(ref gc_lifetime) = self.generics.parsed.gc_lifetime {
            if matches!(kind, TraceDeriveKind::NullTrace) {
                return Err(Error::custom("A NullTrace type should not have a 'gc lifetime").with_span(gc_lifetime))
            }
        }
        self.collector_ids.get_or_insert_with(Default::default);
        self.generics.parsed.normalize()?;
        for tp in &self.generics.parsed.type_params {
            if tp.is_collector_id() {
                self.collector_ids.as_mut().unwrap()
                    .push(tp.ident.clone().into());
            }
        }
        if self.is_copy && self.unsafe_skip_drop {
            return Err(Error::custom("Can't specify both #[zerogc(copy)] and #[zerogc(unsafe_skip_trace)]"))
        }
        if self.is_copy && matches!(kind, TraceDeriveKind::NullTrace) {
            crate::emit_warning("#[zerogc(copy)] is meaningless on NullTrace", self.ident.span())
        }
        Ok(())
    }
    fn gc_lifetime(&self) -> Option<&'_ Lifetime> {
        self.generics.parsed.gc_lifetime.as_ref()
    }
    /// Expand a `GcSafe` for a specific combination of `Id` & 'gc
    ///
    /// Implicitly modifies the specified generics
    fn expand_gcsafe_sepcific(
        &self, kind: TraceDeriveKind,
        initial_generics: Option<Generics>,
        id: &Path, gc_lt: &syn::Lifetime
    ) -> Result<TokenStream, Error> {
        let mut generics = initial_generics.unwrap_or_else(|| self.generics.original.clone());
        let targets = self.generics.parsed.regular_type_params()
            .map(|param| Type::Path(TypePath {
                qself: None,
                path: Path::from(param.ident.clone())
            }))
            .chain(self.determine_field_types());
        for tp in targets {
            match kind {
                TraceDeriveKind::Regular => {
                    generics.make_where_clause().predicates.push(parse_quote!(#tp: zerogc::GcSafe<#gc_lt, #id>));
                },
                TraceDeriveKind::NullTrace => {
                    generics.make_where_clause().predicates.push(
                        parse_quote!(#tp: zerogc::NullTrace)
                    )
                }
            }
        }
        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
        let target_type = &self.ident;
        Ok(quote! {
            unsafe impl #impl_generics zerogc::GcSafe <#gc_lt, #id> for #target_type #ty_generics #where_clause {}
        })
    }
    fn expand_gcsafe(&self, kind: TraceDeriveKind) -> Result<TokenStream, Error> {
        let mut generics = self.generics.original.clone();
        let gc_lifetime: syn::Lifetime = self.gc_lifetime().cloned().unwrap_or_else(|| parse_quote!('gc));
        match kind {
            TraceDeriveKind::NullTrace => {
                // Verify we don't have any explcit collector ids
                if let Some(id) = self.collector_ids.as_ref().and_then(|ids| ids.iter().next()) {
                   return Err(Error::custom("Can't have an explicit CollectorId for NullTrace type").with_span(id))
                }
                generics.params.push(parse_quote!(Id: zerogc::CollectorId));
                self.expand_gcsafe_sepcific(
                    kind, Some(generics),
                    &parse_quote!(Id), &gc_lifetime
                )
            }
            TraceDeriveKind::Regular => {
                let mut has_explicit_collector_ids = false;
                let mut impls = Vec::new();
                if let Some(ref ids) = self.collector_ids {
                    for id in ids {
                        has_explicit_collector_ids = true;
                        let mut initial_generics = generics.clone();
                        initial_generics.make_where_clause().predicates
                            .push(parse_quote!(#id: zerogc::CollectorId));
                        impls.push(self.expand_gcsafe_sepcific(
                            kind, Some(initial_generics),
                            id, &gc_lifetime
                        )?)
                    }
                }
                if !has_explicit_collector_ids {
                    let mut initial_generics = generics.clone();
                    initial_generics.params.push(parse_quote!(Id: zerogc::CollectorId));
                    impls.push(self.expand_gcsafe_sepcific(
                        kind, Some(initial_generics.clone()),
                        &parse_quote!(Id),&gc_lifetime
                    )?)
                }
                assert!(!impls.is_empty());
                Ok(quote!(#(#impls)*))
            }
        }
    }
    fn expand_trace(&self, kind: TraceDeriveKind) -> Result<TokenStream, Error> {
        let target_type = &self.ident;
        if matches!(kind, TraceDeriveKind::NullTrace) {
            let generics = crate::move_bounds_to_where_clause(self.generics.original.clone());
            let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
            return Ok(quote!(zerogc::impl_trace_for_nulltrace!(impl #impl_generics Trace for #target_type #ty_generics #where_clause);))
        }
        let mut generics = self.generics.original.clone();

        for regular in self.generics.parsed.regular_type_params() {
            let name = &regular.ident;
            generics.make_where_clause().predicates.push(parse_quote!(#name: zerogc::Trace))
        }
        let (impl_generics, ty_generics, where_clause) = self.generics.original.split_for_impl();
        let trace_impl = match self.data {
            Data::Enum(ref variants) => {
                let match_arms = variants.iter().map(|v| {
                    let (destructure, access) = v.destructure();
                    let trace_fields = v.fields().enumerate()
                        .map(|(idx, f)| f.expand_trace(idx, &access));
                    let variant_name = &v.ident;
                    quote!(Self::#variant_name #destructure => {
                        #(#trace_fields;)*
                    })
                });
                quote!(match *self {
                    #(#match_arms,)*
                })
            }
            Data::Struct(ref fields) => {
                let access = FieldAccess::SelfMember;
                let trace_fields = fields.iter().enumerate()
                    .map(|(idx, f)| f.expand_trace(idx, &access));
                quote!(#(#trace_fields;)*)
            }
        };
        let has_explicit_drop_impl = if self.unsafe_skip_drop {
            quote!(core::mem::needs_drop::<Self>())
        } else {
            quote!(false)
        };
        let field_types = self.determine_field_types();
        let needs_trace = field_types.iter().map(|ty| quote_spanned!(ty.span() => <#ty as zerogc::Trace>::NEEDS_TRACE));
        let needs_drop = field_types.iter().map(|ty| quote_spanned!(ty.span() => <#ty as zerogc::Trace>::NEEDS_DROP));
        Ok(quote! {
            impl #impl_generics zerogc::Trace for #target_type #ty_generics #where_clause {
                const NEEDS_TRACE: bool = true #(|| #needs_trace)*;
                const NEEDS_DROP: bool =  #has_explicit_drop_impl #(|| #needs_drop)*;
                fn visit<V: zerogc::GcVisitor>(&mut self, gc_visitor: &mut V) -> Result<(), V::Err> {
                    #trace_impl
                    Ok(())
                }
            }
        })
    }
    #[allow(clippy::if_same_then_else)] // Only necessary because of detailed comment
    fn expand_protective_drop(&self, kind: TraceDeriveKind) -> Option<TokenStream> {
        if self.is_copy {
            /*
             * If this type can be proven to implement Copy,
             * it is known to have no destructor.
             * We don't need a fake drop to prevent misbehavior.
             * In fact, adding one would prevent it from being Copy
             * in the first place.
             */
            None
        } else if self.unsafe_skip_drop {
            None // Skip generating drop at user's request
        } else if *self.nop_trace || matches!(kind, TraceDeriveKind::NullTrace) {
            /*
             * A NullTrace type is implicitly drop safe.
             *
             * No tracing needed implies there are no reachable GC references.
             * Therefore there is nothing to fear about finalizers resurrecting things
             */
            None
        } else {
            let target_type = &self.ident;
            let (impl_generics, ty_generics, where_clause) = self.generics.original.split_for_impl();
            Some(quote!(impl #impl_generics Drop for #target_type #ty_generics #where_clause {
                #[inline]
                fn drop(&mut self) {
                    /*
                     * This is only here to prevent the user
                     * from implementing their own Drop functionality.
                     */
                }
            }))
        }
    }
    fn expand_extra_methods(&self, kind: TraceDeriveKind) -> Result<TokenStream, Error> {
        let mut extras = Vec::new();
        match self.data {
            Data::Enum(ref variants) => {
                for v in variants {
                    for f in v.fields() {
                        if f.mutable.is_some() {
                            return Err(Error::custom("NYI: An enum's fields cant be marked #[zerogc(mutable)"))
                        }
                    }
                }
            },
            Data::Struct(ref fields) => {
                for field in &fields.fields {
                    if let Some(ref mutable_opts) = field.mutable {
                        if matches!(kind, TraceDeriveKind::NullTrace) {
                            return Err(Error::custom(
                                "A NullTrace type shouldn't use #[zerogc(mutable)] (just use an ordinary std::cell::Cell)"
                            ))
                        }
                        let original_name = match field.ident {
                            Some(ref name) => name,
                            None => {
                                return Err(Error::custom(
                                    "zerogc can only mutate named fields"
                                ).with_span(&field.ty))
                            }
                        };
                        // Generate a mutator
                        let mutator_name = format_ident!("set_{}", original_name);
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
                                inner_type.ok_or_else(|| Error::custom(
                                    "GcCell should have one (and only one) type param"
                                ).with_span(&field.ty))?
                            },
                            _ => return Err(Error::custom(
                                "A mutable field must be wrapped in a `GcCell`"
                            ).with_span(&field.ty))
                        };
                        // NOTE: Specially quoted since we want to blame the field for errors
                        let field_as_ptr = quote_spanned!(original_name.span() => zerogc::cell::GcCell::as_ptr(&(*self.value()).#original_name));
                        let barrier = quote_spanned!(original_name.span() => zerogc::GcDirectBarrier::write_barrier(&value, &self, offset));
                        let mut id_type = None;
                        let mut method_generics = Generics::default();
                        for id in self.collector_ids.iter().flat_map(|ids| ids.iter()) {
                            if id_type.is_some() {
                                return Err(Error::custom("NYI: #[field(mutable)] cannot currently have multiple CollectorIds (unless generic over *all* of them)").with_span(&field.ty))
                            }
                            method_generics.make_where_clause().predicates.push(parse_quote!(#id: zerogc::CollectorId));
                            id_type = Some(id.clone());
                        }
                        let id_type = match id_type {
                            Some(tp) => {
                                method_generics.params.push(parse_quote!(#tp: zerogc::CollectorId));
                                tp
                            },
                            None => {
                                method_generics.params.push(parse_quote!(Id: zerogc::CollectorId));
                                parse_quote!(Id)
                            }
                        };
                        let gc_lifetime = match self.gc_lifetime() {
                            Some(lt) => lt.clone(),
                            None => {
                                method_generics.params.push(parse_quote!('gc));
                                parse_quote!('gc)
                            }
                        };
                        method_generics.make_where_clause().predicates.push(parse_quote!(
                            #value_ref_type: zerogc::GcDirectBarrier<
                                #gc_lifetime,
                                zerogc::Gc<#gc_lifetime, Self, #id_type>
                            >
                        ));
                        let where_clause = &method_generics.where_clause;
                        extras.push(quote! {
                            #[inline] // TODO: Implement `GcDirectBarrier` ourselves
                            #mutator_vis fn #mutator_name #method_generics(self: zerogc::Gc<#gc_lifetime, Self, #id_type>, value: #value_ref_type)
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
            }
        }
        let (impl_generics, ty_generics, where_clause) = self.generics.original.split_for_impl();
        let target_type = &self.ident;
        Ok(quote! {
            impl #impl_generics #target_type #ty_generics #where_clause {
                #(#extras)*
            }
        })
    }
    pub fn expand(&self, kind: TraceDeriveKind) -> Result<TokenStream, Error> {
        let gcsafe = self.expand_gcsafe(kind)?;
        let trace = self.expand_trace(kind)?;
        let protective_drop = self.expand_protective_drop(kind);
        let extra_methods = self.expand_extra_methods(kind)?;
        Ok(quote! {
            #gcsafe
            #trace
            #protective_drop
            #extra_methods
        })
    }
}

pub enum FieldAccess {
    None,
    SelfMember,
    Variable {
        prefix: Option<Ident>
    }
}
impl FieldAccess {
    fn access_named_field(&self, name: Ident) -> TokenStream {
        match *self {
            FieldAccess::None => unreachable!("no fields"),
            FieldAccess::SelfMember => quote_spanned!(name.span() => self.#name),
            FieldAccess::Variable { prefix: Some(ref prefix) } => {
                format_ident!("{}{}", prefix, name).into_token_stream()
            },
            FieldAccess::Variable { prefix: None } => {
                name.into_token_stream()
            }
        }
    }

    fn access_indexed_field(&self, idx: usize, span: Span) -> TokenStream {
        match *self {
            Self::None => unreachable!("no fields"),
            Self::SelfMember => {
                let idx = syn::Index { index: idx as u32, span };
                quote_spanned!(span => self.#idx)
            },
            Self::Variable { prefix: Some(ref prefix) } => {
                format_ident!("{}{}", span = span, prefix, idx).into_token_stream()
            },
            Self::Variable { prefix: None } => unreachable!("Can't access index fields without a prefix")
        }
    }
}