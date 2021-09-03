use darling::{Error, FromMeta, FromGenerics, FromTypeParam, FromDeriveInput, FromVariant, FromField};
use proc_macro2::{Ident, TokenStream, Span};
use syn::{Generics, Type, GenericParam, TypeParam, Lifetime, Path, parse_quote, PathArguments, GenericArgument, TypePath, Meta, LifetimeDef};
use darling::util::{SpannedValue};
use quote::{quote_spanned, quote, format_ident, ToTokens};
use darling::ast::{Style, Data};
use std::collections::HashSet;
use syn::spanned::Spanned;

use crate::{FromLitStr, MetaList};

#[derive(Copy, Clone, Debug)]
pub enum TraceDeriveKind {
    NullTrace,
    Regular,
    Deserialize
}

trait PossiblyIgnoredParam {
    fn check_ignored(&self, generics: &TraceGenerics) -> bool;
}
impl PossiblyIgnoredParam for syn::Lifetime {
    fn check_ignored(&self, generics: &TraceGenerics) -> bool {
        generics.ignored_lifetimes.contains(self)
    }
}
impl PossiblyIgnoredParam for Ident {
    fn check_ignored(&self, generics: &TraceGenerics) -> bool {
        generics.type_params.iter().any(|param| (param.ignore || param.collector_id) && param.ident == *self)
    }
}
impl PossiblyIgnoredParam for Path {
    fn check_ignored(&self, generics: &TraceGenerics) -> bool {
        self.get_ident().map_or(false, |ident| ident.check_ignored(generics))
    }
}
impl PossiblyIgnoredParam for syn::Type {
    fn check_ignored(&self, generics: &TraceGenerics) -> bool {
        match *self {
            Type::Group(ref e) => e.elem.check_ignored(generics),
            Type::Paren(ref p) => p.elem.check_ignored(generics),
            Type::Path(ref p) if p.qself.is_none() => {
                p.path.check_ignored(generics)
            },
            _ => false
        }
    }
}

#[derive(Debug)]
struct TraceGenerics {
    original: Generics,
    gc_lifetime: Option<Lifetime>,
    ignored_lifetimes: HashSet<Lifetime>,
    type_params: Vec<TraceTypeParam>
}
impl TraceGenerics {
    fn is_ignored<P: PossiblyIgnoredParam>(&self, item: &P) -> bool {
        item.check_ignored(self)
    }
    /// Return all the "regular" type parameters,
    /// excluding `Id` parameters and  those marked `#[zerogc(ignore)]`
    fn regular_type_params(&self) -> impl Iterator<Item=&'_ TraceTypeParam> + '_ {
        self.type_params.iter().filter(|param| !param.ignore && !param.collector_id)
    }
    fn normalize(&mut self) -> Result<(), Error> {
        for tp in &mut self.type_params {
            tp.normalize()?;
        }
        if let Some(ref gc) = self.gc_lifetime {
            if self.ignored_lifetimes.contains(gc) {
                return Err(Error::custom("Attribute can't be a gc_lifetime but also be ignored")
                    .with_span(gc))
            }
        }
        if let (&None, Some(gc)) = (&self.gc_lifetime, self.original.lifetimes()
            .find(|lt| lt.lifetime.ident == "gc")) {
            self.gc_lifetime = Some(gc.lifetime.clone());
        }
        for lt in self.original.lifetimes() {
            let lt = &lt.lifetime;
            let ignored = self.ignored_lifetimes.contains(lt);
            let is_gc = Some(lt) == self.gc_lifetime.as_ref();
            if !ignored && !is_gc {
                return Err(Error::custom("Lifetime must be either ignored or 'gc").with_span(lt))
            }
        }
        Ok(())
    }
    fn _handle_type(&mut self, tp: &TypeParam) -> Result<(), Error> {
        self.type_params.push(TraceTypeParam::from_type_param(tp)?);
        Ok(())
    }
}
impl FromGenerics for TraceGenerics {
    fn from_generics(generics: &Generics) -> Result<Self, Error> {
        let mut res = TraceGenerics {
            original: generics.clone(),
            gc_lifetime: None,
            ignored_lifetimes: Default::default(),
            type_params: Vec::new()
        };
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
                GenericParam::Lifetime(_) => {},

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
    #[darling(skip)]
    ignore: bool,
    #[darling(skip)]
    collector_id: bool,
}
impl TraceTypeParam {
    fn normalize(&mut self) -> Result<(), Error> {
        if self.ignore && self.collector_id {
            return Err(Error::custom("Type parameter can't be ignored but also collector_id"))
        }
        if self.ident == "Id" && !self.collector_id {
            crate::emit_warning(
                "Type parameter is named `Id` but isn't marked #[zerogc(collector_id)]. Specify collector_id = false if this is intentional.",
                self.ident.span()
            )
        }
        Ok(())
    }
}

#[derive(Debug, FromMeta, Default)]
struct MutableFieldOptions {
    #[darling(default)]
    public: bool,
}

#[derive(Debug, Clone, Default)]
struct FromOrDefault<T>(pub T);
impl<T: FromMeta + Default> FromMeta for FromOrDefault<T> {
    fn from_meta(item: &Meta) -> darling::Result<Self> {
        if let Meta::Path(_) = item {
            Ok(Default::default())
        } else {
            T::from_meta(item).map(FromOrDefault)
        }
    }

    fn from_word() -> darling::Result<Self> {
        Ok(Default::default())
    }


}

#[derive(Debug, FromField)]
#[darling(attributes(zerogc))]
struct TraceField {
    ident: Option<Ident>,
    ty: Type,
    #[darling(default)]
    mutable: Option<FromOrDefault<MutableFieldOptions>>,
    /// Unsafely skip tracing the field.
    ///
    /// Undefined behavior if the type actually needs
    /// to be traced.
    #[darling(default)]
    unsafe_skip_trace: bool,

    #[darling(forward_attrs(serde))]
    attrs: Vec<syn::Attribute>
}
impl TraceField {
    fn expand_trace(&self, idx: usize, access: &FieldAccess, immutable: bool) -> TokenStream {
        let access = match self.ident {
            Some(ref name) => access.access_named_field(name.clone()),
            None => access.access_indexed_field(idx, self.ty.span())
        };
        let ty = &self.ty;
        let trait_name: Path = if immutable {
            parse_quote!(zerogc::TraceImmutable)
        } else {
            parse_quote!(zerogc::Trace)
        };
        let method_name: Ident = if immutable {
            parse_quote!(visit_immutable)
        } else {
            parse_quote!(visit)
        };
        quote_spanned!(self.ty.span() => <#ty as #trait_name>::#method_name(#access, gc_visitor)?)
    }
}

#[derive(Debug, FromVariant)]
#[darling(attributes(zerogc))]
struct TraceVariant {
    ident: Ident,
    fields: darling::ast::Fields<TraceField>,
    #[darling(forward_attrs(serde))]
    attrs: Vec<syn::Attribute>
}
impl TraceVariant {
    fn fields(&self) -> impl Iterator<Item=&'_ TraceField> + '_ {
        self.fields.iter()
    }
    pub fn destructure(&self, immutable: bool) -> (TokenStream, FieldAccess) {
        let mutability = if !immutable { Some(<syn::Token![mut]>::default()) } else { None };
        match self.fields.style {
            Style::Unit => (quote!(), FieldAccess::None),
            Style::Tuple => {
                let prefix: Ident = parse_quote!(member);
                let destructure = self.fields.iter().enumerate().map(|(idx, field)| {
                    let name = format_ident!("member{}", span = field.ty.span(), idx);
                    quote!(ref #mutability #name)
                });
                (quote!((#(#destructure),*)), FieldAccess::Variable {
                    prefix: Some(prefix)
                })
            },
            Style::Struct => {
                let fields = self.fields.iter()
                    .map(|field| field.ident.as_ref().unwrap());
                (quote!({ #(ref #mutability #fields),* }), FieldAccess::Variable { prefix: None })
            }
        }
    }
}

#[derive(FromDeriveInput)]
#[darling(attributes(zerogc))]
pub struct TraceDeriveInput {
    pub ident: Ident,
    generics: TraceGenerics,
    data: darling::ast::Data<TraceVariant, TraceField>,
    // TODO: Eventually remove this option
    #[darling(default)]
    nop_trace: SpannedValue<bool>,
    #[darling(default)]
    ignore_lifetimes: MetaList<FromLitStr<syn::Lifetime>>,
    #[darling(default)]
    ignore_params: MetaList<Ident>,
    #[darling(default)]
    collector_ids: Option<MetaList<Path>>,
    /// Unsafely assume the type is safe to [drop]
    /// from a GC, as consistent with the requirements of [GcSafe]
    ///
    /// This 'skips' the generation of a dummy drop
    #[darling(default)]
    unsafe_skip_drop: bool,
    #[darling(default, rename = "copy")]
    is_copy: bool,
    /// If the type should implement `TraceImmutable` in addition to `Trace
    #[darling(default, rename = "immutable")]
    wants_immutable_trace: bool,
    #[darling(forward_attrs(serde))]
    attrs: Vec<syn::Attribute>
}
impl TraceDeriveInput {
    fn all_fields(&self) -> Vec<&TraceField> {
        match self.data {
            Data::Enum(ref variants) => {
                variants.iter().flat_map(|var| var.fields()).collect()
            },
            Data::Struct(ref fields) => fields.iter().collect()
        }
    }
    pub fn determine_field_types(&self, include_ignored: bool) -> HashSet<Type> {
        self.all_fields().iter()
            .filter(|f| !f.unsafe_skip_trace || include_ignored)
            .map(|fd| &fd.ty)
            .cloned()
            .collect()
    }
    pub fn normalize(&mut self, kind: TraceDeriveKind) -> Result<(), Error> {
        if *self.nop_trace {
            crate::emit_warning("#[zerogc(nop_trace)] is deprecated (use #[derive(NullTrace)] instead)", self.nop_trace.span())
        }
        if let Some(ref gc_lifetime) = self.generics.gc_lifetime {
            if matches!(kind, TraceDeriveKind::NullTrace) {
                return Err(Error::custom("A NullTrace type should not have a 'gc lifetime").with_span(gc_lifetime))
            }
        }
        self.collector_ids.get_or_insert_with(Default::default);
        for ignored in &self.ignore_params.0 {
            let tp = self.generics.type_params.iter_mut()
                .find(|param| &param.ident == ignored)
                .ok_or_else(|| Error::custom("Unknown type parameter").with_span(&ignored))?; 
            if tp.ignore {
                return Err(Error::custom("Already marked as a collector id").with_span(&ignored))
            }
            tp.ignore = true;
        }
        self.generics.ignored_lifetimes.extend(self.ignore_lifetimes.0.iter().map(|lt| lt.0.clone()));
        for id in self.collector_ids.as_ref().unwrap().0.iter() {
            if let Some(tp) = self.generics.type_params.iter_mut()
                .find(|param| id.is_ident(&param.ident)) {
                if tp.collector_id {
                    return Err(Error::custom("Already marked as collector id").with_span(&id))
                }
                tp.collector_id = true;
            }
        }
        self.generics.normalize()?;

        if self.is_copy && self.unsafe_skip_drop {
            return Err(Error::custom("Can't specify both #[zerogc(copy)] and #[zerogc(unsafe_skip_trace)]"))
        }
        if self.is_copy && matches!(kind, TraceDeriveKind::NullTrace) {
            crate::emit_warning("#[zerogc(copy)] is meaningless on NullTrace", self.ident.span())
        }
        Ok(())
    }
    fn gc_lifetime(&self) -> Option<&'_ Lifetime> {
        self.generics.gc_lifetime.as_ref()
    }
    fn generics_with_gc_lifetime(&self, lt: Lifetime) -> (syn::Lifetime, Generics) {
        let mut generics = self.generics.original.clone();
        let gc_lifetime: syn::Lifetime = match self.gc_lifetime() {
            Some(lt) => lt.clone(),
            None => {
                generics.params.push(GenericParam::Lifetime(LifetimeDef::new(lt.clone())));
                lt
            }
        };
        (gc_lifetime, generics)
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
        let targets = self.generics.regular_type_params()
            .map(|param| Type::Path(TypePath {
                qself: None,
                path: Path::from(param.ident.clone())
            }))
            .chain(self.determine_field_types(false));
        let requirement = match kind {
            TraceDeriveKind::Regular => {
                quote!(zerogc::GcSafe<#gc_lt, #id>)
            },
            TraceDeriveKind::NullTrace => {
                quote!(zerogc::NullTrace)
            },
            TraceDeriveKind::Deserialize => unreachable!()
        };
        for tp in self.generics.regular_type_params() {
            let tp = &tp.ident;
            match kind {
                TraceDeriveKind::Regular => {
                    generics.make_where_clause().predicates.push(parse_quote!(#tp: #requirement));
                },
                TraceDeriveKind::NullTrace => {
                    generics.make_where_clause().predicates.push(
                        parse_quote!(#tp: #requirement)
                    )
                },
                TraceDeriveKind::Deserialize => unreachable!()
            }
        }
        let assertion: Ident = match kind {
            TraceDeriveKind::NullTrace => parse_quote!(verify_null_trace),
            TraceDeriveKind::Regular => parse_quote!(assert_gc_safe),
            TraceDeriveKind::Deserialize => unreachable!()
        };
        let ty_generics = self.generics.original.split_for_impl().1;
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        let target_type = &self.ident;
        Ok(quote! {
            unsafe impl #impl_generics zerogc::GcSafe <#gc_lt, #id> for #target_type #ty_generics #where_clause {
                fn assert_gc_safe() -> bool {
                    #(<#targets as #requirement>::#assertion();)*
                    true
                }
            }
        })
    }
    fn expand_gcsafe(&self, kind: TraceDeriveKind) -> Result<TokenStream, Error> {
        let (gc_lifetime, mut generics) = self.generics_with_gc_lifetime(parse_quote!('gc));
        match kind {
            TraceDeriveKind::NullTrace => {
                // Verify we don't have any explicit collector id
                if let Some(id) = self.collector_ids.as_ref().and_then(|ids| ids.0.iter().next()) {
                   return Err(Error::custom("Can't have an explicit CollectorId for NullTrace type").with_span(id))
                }
                generics.params.push(parse_quote!(Id: zerogc::CollectorId));
                self.expand_gcsafe_sepcific(
                    kind, Some(generics),
                    &parse_quote!(Id), &gc_lifetime
                )
            }
            TraceDeriveKind::Regular => {
                self.expand_for_each_regular_id(
                    generics.clone(), kind, gc_lifetime,
                    &mut |kind, initial, id, gc_lt| {
                        self.expand_gcsafe_sepcific(kind, initial, id, gc_lt)
                    }
                )
            },
            TraceDeriveKind::Deserialize => unreachable!()
        }
    }

    fn expand_deserialize(&self) -> Result<TokenStream, Error> {
        if !crate::DESERIALIZE_ENABLED {
            return Err(Error::custom("The `zerogc/serde1` feature is disabled (please enable it)"));
        }
        let (gc_lifetime, generics) = self.generics_with_gc_lifetime(parse_quote!('gc));
        self.expand_for_each_regular_id(
            generics, TraceDeriveKind::Deserialize, gc_lifetime,
            &mut |kind, initial, id, gc_lt| {
                assert!(matches!(kind, TraceDeriveKind::Deserialize));
                let id_is_generic = self.generics.original.type_params()
                    .any(|param| id.is_ident(&param.ident));
                let mut generics = initial.unwrap();
                generics.params.push(parse_quote!('deserialize));
                let requirement = quote!(for<'deser2> zerogc::serde::GcDeserialize::<#gc_lt, 'deser2, #id>);
                for target in self.generics.regular_type_params() {
                    let target = &target.ident;
                    generics.make_where_clause().predicates.push(parse_quote!(#target: #requirement));
                }
                let ty_generics = self.generics.original.split_for_impl().1;
                let (impl_generics, _, where_clause) = generics.split_for_impl();
                let target_type = &self.ident;
                let forward_attrs = &self.attrs;
                let deserialize_field = |f: &TraceField| {
                    let named = f.ident.as_ref().map(|name| quote!(#name: ));
                    let ty = &f.ty;
                    let forwarded_attrs = &f.attrs;
                    let bound = format!(
                        "{}: for<'deserialize> zerogc::serde::GcDeserialize<{}, 'deserialize, {}>", ty.to_token_stream(),
                        gc_lt.to_token_stream(), id.to_token_stream()
                    );
                    quote! {
                        #(#forwarded_attrs)*
                        # [serde(deserialize_with = "deserialize_hack", bound(deserialize = #bound))]
                        #named #ty
                    }
                };
                let handle_fields = |fields: &darling::ast::Fields<TraceField>| {
                    let handled_fields = fields.fields.iter().map(deserialize_field);
                    match fields.style {
                        Style::Tuple => {
                            quote!{ ( #(#handled_fields),* ) }
                        }
                        Style::Struct => {
                            quote!({ #(#handled_fields),* })
                        }
                        Style::Unit => quote!()
                    }
                };
                let original_generics = &self.generics.original;
                let inner = match self.data {
                    Data::Enum(ref variants) => {
                        let variants = variants.iter().map(|v| {
                            let forward_attrs = &v.attrs;
                            let name = &v.ident;
                            let inner = handle_fields(&v.fields);
                            quote! {
                                #(#forward_attrs)*
                                #name #inner
                            }
                        });
                        quote!(enum HackRemoteDeserialize #original_generics { #(#variants),* })
                    }
                    Data::Struct(ref f) => {
                        let fields = handle_fields(f);
                        quote!(struct HackRemoteDeserialize #original_generics # fields)
                    }
                };
                let remote_name = target_type.to_token_stream().to_string();
                let id_decl = if id_is_generic {
                    Some(quote!(#id: zerogc::CollectorId,))
                } else { None };
                Ok(quote! {
                    impl #impl_generics zerogc::serde::GcDeserialize<#gc_lt, 'deserialize, #id> for #target_type #ty_generics #where_clause {
                        fn deserialize_gc<D: serde::Deserializer<'deserialize>>(ctx: &#gc_lt <<#id as zerogc::CollectorId>::System as zerogc::GcSystem>::Context, deserializer: D) -> Result<Self, D::Error> {
                            use serde::Deserializer;
                            let _guard = unsafe { zerogc::serde::hack::set_context(ctx) };
                            unsafe {
                                debug_assert_eq!(_guard.get_unchecked() as *const _, ctx as *const _);
                            }
                            /// Hack function to deserialize via `serde::hack`, with the appropriate `Id` type
                            ///
                            /// Needed because the actual function is unsafe
                            #[track_caller]
                            fn deserialize_hack<'gc, 'de, #id_decl D: serde::de::Deserializer<'de>, T: zerogc::serde::GcDeserialize<#gc_lt, 'de, #id>>(deser: D) -> Result<T, D::Error> {
                                unsafe { zerogc::serde::hack::unchecked_deserialize_hack::<'gc, 'de, D, #id, T>(deser) }
                            }
                            # [derive(serde::Deserialize)]
                            # [serde(remote = #remote_name)]
                            #(#forward_attrs)*
                            #inner ;
                            HackRemoteDeserialize::deserialize(deserializer)
                        }
                    }
                })
            }
        )
    }
    fn expand_for_each_regular_id(
        &self, generics: Generics,
        kind: TraceDeriveKind,
        gc_lifetime: Lifetime,
        func: &mut dyn FnMut(TraceDeriveKind, Option<Generics>, &Path, &Lifetime) -> Result<TokenStream, Error>
    ) -> Result<TokenStream, Error> {
        let mut has_explicit_collector_ids = false;
        let mut impls = Vec::new();
        if let Some(ref ids) = self.collector_ids {
            for id in ids.0.iter() {
                has_explicit_collector_ids = true;
                let mut initial_generics = generics.clone();
                initial_generics.make_where_clause().predicates
                    .push(parse_quote!(#id: zerogc::CollectorId));
                impls.push(func(
                    kind, Some(initial_generics),
                    id, &gc_lifetime
                )?)
            }
        }
        if !has_explicit_collector_ids {
            let mut initial_generics = generics;
            initial_generics.params.push(parse_quote!(Id: zerogc::CollectorId));
            impls.push(func(
                kind, Some(initial_generics.clone()),
                &parse_quote!(Id),&gc_lifetime
            )?)
        }
        assert!(!impls.is_empty());
        Ok(quote!(#(#impls)*))
    }
    fn expand_rebrand(&self, kind: TraceDeriveKind) -> Result<TokenStream, Error> {
        let target_type = &self.ident;
        if matches!(kind, TraceDeriveKind::NullTrace) {
            let mut generics = self.generics.original.clone();
            generics.params.push(parse_quote!(Id: zerogc::CollectorId));
            generics.params.push(parse_quote!('new_gc));
            for regular in self.generics.regular_type_params() {
                let regular = &regular.ident;
                generics.make_where_clause().predicates.push(parse_quote!(#regular: zerogc::NullTrace));
            }
            for ignored in &self.generics.ignored_lifetimes {
                generics.make_where_clause().predicates.push(parse_quote!(#ignored: 'new_gc))
            }
            generics.make_where_clause().predicates.push(parse_quote!(Self: GcSafe<'new_gc, Id>));
            if let Some(ref gc_lt) = self.gc_lifetime() {
                return Err(Error::custom("A NullTrace type may not have a 'gc lifetime").with_span(gc_lt))
            }
            let (_, ty_generics, _) = self.generics.original.split_for_impl();
            let (impl_generics, _, where_clause) = generics.split_for_impl();
            return Ok(quote! {
                unsafe impl #impl_generics zerogc::GcRebrand<'new_gc, Id> for #target_type #ty_generics #where_clause {
                    type Branded = Self;
                }
            });
        }
        let mut generics = self.generics.original.clone();
        let (old_gc_lt, new_gc_lt): (syn::Lifetime, syn::Lifetime) = match self.gc_lifetime() {
            Some(lt) => {
                generics.params.push(parse_quote!('new_gc));
                (lt.clone(), parse_quote!('new_gc))
            },
            None => {
                generics.params.push(parse_quote!('gc));
                generics.params.push(parse_quote!('new_gc));
                (parse_quote!('gc), parse_quote!('new_gc))
            }
        };
        self.expand_for_each_regular_id(
            generics, kind, old_gc_lt,
            &mut |kind, initial_generics, id, orig_lt| {
                self.expand_rebrand_specific(
                    kind, initial_generics,
                    id, orig_lt, new_gc_lt.clone()
                )
            }
        )
    }
    fn expand_rebrand_specific(
        &self, kind: TraceDeriveKind,
        initial_generics: Option<Generics>,
        id: &Path, orig_lt: &Lifetime, new_lt: Lifetime
    ) -> Result<TokenStream, Error> {
        assert!(!matches!(kind, TraceDeriveKind::NullTrace));
        let mut fold = RebrandFold {
            generics: &self.generics,
            collector_ids: self.collector_ids.as_ref().map_or_else(
                || HashSet::from([parse_quote!(Id)]),
                |ids| {
                ids.0.iter().map(|p| Type::Path(TypePath {
                    qself: None,
                    path: p.clone()
                })).collect()
            }),
            new_lt: &new_lt,
            orig_lt, id
        };
        let mut generics = syn::fold::fold_generics(
            &mut fold,
            initial_generics.unwrap_or_else(|| self.generics.original.clone()),
        );
        for param in self.generics.regular_type_params() {
            let name = &param.ident;
            generics.make_where_clause().predicates.push(parse_quote!(#name: zerogc::GcRebrand<#new_lt, #id>));
            let rewritten_bounds = param.bounds.iter().cloned().map(|bound| {
                syn::fold::fold_type_param_bound(&mut ReplaceLt {
                    orig_lt, new_lt: &new_lt
                }, bound)
            }).collect::<Vec<_>>();
            generics.make_where_clause().predicates.push(parse_quote!(#name::Branded: #(#rewritten_bounds)+*));
        }
        for ignored in &self.generics.ignored_lifetimes {
            generics.make_where_clause().predicates.push(parse_quote!(#ignored: 'new_gc));
        }
        let target_type = &self.ident;

        let rewritten_path: Path = {
            let mut params = self.generics.original.params.iter().map(|decl| {
                // decl -> use
                match decl {
                    GenericParam::Type(ref tp) => {
                        let name = &tp.ident;
                        parse_quote!(#name)
                    }
                    GenericParam::Lifetime(ref lt) => {
                        let name = fold.rewrite_lifetime(lt.lifetime.clone());
                        parse_quote!(#name)
                    },
                    GenericParam::Const(ref c) => {
                        let name = &c.ident;
                        parse_quote!(#name)
                    }
                }
            }).collect::<Vec<GenericArgument>>();
            params = params.into_iter().map(|arg| {
                syn::fold::fold_generic_argument(&mut fold, arg)
            }).collect();

            parse_quote!(#target_type::<#(#params),*>)
        };
        let explicitly_unsized = self.generics.original.params.iter()
            .filter_map(|param| match param {
                GenericParam::Type(ref t) => Some(t),
                _ => None
            })
            .filter(|&param| crate::is_explicitly_unsized(param))
            .map(|param| param.ident.clone())
            .collect::<HashSet<Ident>>();
        // Add the appropriate T::Branded: Sized bounds
        for param in self.generics.regular_type_params() {
            let name = &param.ident;
            if explicitly_unsized.contains(name) { continue }
            generics.make_where_clause().predicates.push(parse_quote!(#name::Branded: Sized));
        }
        let ty_generics = self.generics.original.split_for_impl().1;
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        Ok(quote! {
            unsafe impl #impl_generics zerogc::GcRebrand<#new_lt, #id> for #target_type #ty_generics #where_clause {
                type Branded = #rewritten_path;
            }
        })
    }
    fn expand_trace(&self, kind: TraceDeriveKind, immutable: bool) -> Result<TokenStream, Error> {
        let target_type = &self.ident;
        if matches!(kind, TraceDeriveKind::NullTrace) {
            if immutable {
                return Err(Error::custom("A `NullTrace` type should not be explicitly marked #[zerogc(immutable)] (it's implied)"))
            }
            let generics = crate::move_bounds_to_where_clause(self.generics.original.clone());
            let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
            return Ok(quote! {
                unsafe impl #impl_generics zerogc::NullTrace for #target_type #ty_generics #where_clause {}
                zerogc::impl_trace_for_nulltrace!(impl #impl_generics Trace for #target_type #ty_generics #where_clause);
            })
        }
        let trait_name: Path = if immutable {
            parse_quote!(zerogc::TraceImmutable)
        } else {
            parse_quote!(zerogc::Trace)
        };
        let method_name: Ident = if immutable {
            parse_quote!(visit_immutable)
        } else {
            parse_quote!(visit)
        };
        let mut generics = self.generics.original.clone();
        for regular in self.generics.regular_type_params() {
            let name = &regular.ident;
            generics.make_where_clause().predicates.push(parse_quote!(#name: #trait_name))
        }
        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
        let trace_impl = match self.data {
            Data::Enum(ref variants) => {
                let match_arms = variants.iter().map(|v| {
                    let (destructure, access) = v.destructure(immutable);
                    let trace_fields = v.fields().enumerate()
                        .filter(|(_, f)| !f.unsafe_skip_trace)
                        .map(|(idx, f)| f.expand_trace(idx, &access, immutable));
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
                let access = FieldAccess::SelfMember { immutable };
                let trace_fields = fields.iter().enumerate()
                    .filter(|(_, f)| !f.unsafe_skip_trace)
                    .map(|(idx, f)| f.expand_trace(idx, &access, immutable));
                quote!(#(#trace_fields;)*)
            }
        };
        let has_explicit_drop_impl = if self.unsafe_skip_drop {
            quote!(core::mem::needs_drop::<Self>())
        } else {
            quote!(false)
        };
        let traced_field_types = self.determine_field_types(false);
        let all_field_types = self.determine_field_types(true);
        let needs_trace = traced_field_types.iter().map(|ty| quote_spanned!(ty.span() => <#ty as zerogc::Trace>::NEEDS_TRACE));
        let needs_drop = all_field_types.iter().map(|ty| quote_spanned!(ty.span() => <#ty as zerogc::Trace>::NEEDS_DROP));
        let assoc_constants = if !immutable {
            Some(quote! {
                const NEEDS_TRACE: bool = #(#needs_trace || )* false /* NOTE: Default to *false* if we have no GC types inside */;
                const NEEDS_DROP: bool =  #has_explicit_drop_impl #(|| #needs_drop)*;
            })
        } else {
            None
        };
        let visit_inside_gc = if !immutable {
            let where_clause = quote!(where Visitor: zerogc::GcVisitor,
                ActualId: zerogc::CollectorId, Self: zerogc::GcSafe<'actual_gc, ActualId> + 'actual_gc);
            Some(quote! {
                #[inline]
                unsafe fn visit_inside_gc<'actual_gc, Visitor, ActualId>(gc: &mut zerogc::Gc<'actual_gc, Self, ActualId>, visitor: &mut Visitor) -> Result<(), Visitor::Err>
                #where_clause {
                    visitor.visit_gc(gc)
                }
            })
        } else { None };
        let mutability = if !immutable { Some(<syn::Token![mut]>::default()) } else { None };
        Ok(quote! {
            unsafe impl #impl_generics #trait_name for #target_type #ty_generics #where_clause {
                #assoc_constants
                #visit_inside_gc
                fn #method_name<V: zerogc::GcVisitor>(&#mutability self, gc_visitor: &mut V) -> Result<(), V::Err> {
                    #trace_impl
                    Ok(())
                }
            }
        })
    }
    fn expand_trusted_drop(&self, kind: TraceDeriveKind) -> TokenStream {
        let mut generics = self.generics.original.clone();
        for param in self.generics.regular_type_params() {
            let name = &param.ident;
            generics.make_where_clause().predicates.push(parse_quote!(#name: zerogc::TrustedDrop));
        }
        #[allow(clippy::if_same_then_else)] // Only necessary because of detailed comment
        let protective_drop = if self.is_copy {
            /*
             * If this type can be proven to implement Copy,
             * it is known to have no destructor.
             * We don't need a fake drop to prevent misbehavior.
             * In fact, adding one would prevent it from being Copy
             * in the first place.
             */
            None // TODO: Actually verify that we are in fact `Copy`
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
        };        
        let target_type = &self.ident;
        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
        quote! {
            #protective_drop
            unsafe impl #impl_generics zerogc::TrustedDrop for #target_type #ty_generics #where_clause {}
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
                        let mutator_vis = if mutable_opts.0.public {
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
                        for id in self.collector_ids.iter().flat_map(|ids| ids.0.iter()) {
                            if id_type.is_some() {
                                return Err(Error::custom("NYI: #[field(mutable)] cannot currently have multiple CollectorIds (unless generic over *all* of them)").with_span(&field.ty))
                            }
                            method_generics.make_where_clause().predicates.push(parse_quote!(#id: zerogc::CollectorId));
                            id_type = Some(id.clone());
                        }
                        let id_type = match id_type {
                            Some(tp) => {
                                method_generics.make_where_clause().predicates
                                .push(parse_quote!(#tp: zerogc::CollectorId));
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
        if matches!(kind, TraceDeriveKind::Deserialize) {
            return self.expand_deserialize();
        }
        let gcsafe = self.expand_gcsafe(kind)?;
        let trace_immutable = if self.wants_immutable_trace {
            Some(self.expand_trace(kind, true)?)
        } else {
            None
        };
        let rebrand = self.expand_rebrand(kind)?;
        let trace = self.expand_trace(kind, false)?;
        let protective_drop = self.expand_trusted_drop(kind);
        let extra_methods = self.expand_extra_methods(kind)?;
        Ok(quote! {
            #rebrand
            #gcsafe
            #trace_immutable
            #trace
            #protective_drop
            #extra_methods
        })
    }
}

pub enum FieldAccess {
    None,
    SelfMember {
        immutable: bool
    },
    Variable {
        prefix: Option<Ident>
    }
}
impl FieldAccess {
    fn access_named_field(&self, name: Ident) -> TokenStream {
        match *self {
            FieldAccess::None => unreachable!("no fields"),
            FieldAccess::SelfMember { immutable: false } => quote_spanned!(name.span() => &mut self.#name),
            FieldAccess::SelfMember { immutable: true } => quote_spanned!(name.span() => &self.#name),
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
            Self::SelfMember { immutable: false } => {
                let idx = syn::Index { index: idx as u32, span };
                quote_spanned!(span => &mut self.#idx)
            },
            Self::SelfMember { immutable: true } => {
                let idx = syn::Index { index: idx as u32, span };
                quote_spanned!(span => &self.#idx)
            }
            Self::Variable { prefix: Some(ref prefix) } => {
                format_ident!("{}{}", span = span, prefix, idx).into_token_stream()
            },
            Self::Variable { prefix: None } => unreachable!("Can't access index fields without a prefix")
        }
    }
}

struct ReplaceLt<'a> {
    orig_lt: &'a syn::Lifetime,
    new_lt: &'a syn::Lifetime
}
impl syn::fold::Fold for ReplaceLt<'_> {
    fn fold_lifetime(&mut self, orig: Lifetime) -> Lifetime {
        if orig == *self.orig_lt {
            self.new_lt.clone()
        } else {
            orig
        }
    }
}
struct RebrandFold<'a> {
    generics: &'a TraceGenerics,
    collector_ids: HashSet<Type>,
    new_lt: &'a syn::Lifetime,
    orig_lt: &'a syn::Lifetime,
    id: &'a syn::Path
}
impl RebrandFold<'_> {
    fn rewrite_lifetime(&self, orig: Lifetime) -> Lifetime {
        if orig == *self.orig_lt {
            self.new_lt.clone()
        } else {
            orig
        }
    }
}
impl<'a> syn::fold::Fold for RebrandFold<'a> {
    fn fold_lifetime_def(&mut self, orig: LifetimeDef) -> LifetimeDef {
        LifetimeDef {
            bounds: orig.bounds.into_iter().map(|lt| self.rewrite_lifetime(lt)).collect(),
            colon_token: orig.colon_token,
            lifetime: orig.lifetime,
            attrs: orig.attrs
        }
    }

    fn fold_type(&mut self, orig: Type) -> Type {
        if self.generics.is_ignored(&orig) || self.collector_ids.contains(&orig) {
            orig
        } else {
            let new_lt = self.new_lt;
            let id = self.id;
            parse_quote!(<#orig as zerogc::GcRebrand<#new_lt, #id>>::Branded)
        }
    }
}