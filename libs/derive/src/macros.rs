//! Procedural macros for implementing `GcType`

/*
 * The main macro here is `unsafe_impl_gc`
 */

use proc_macro2::{Ident, TokenStream, TokenTree};
use syn::{
    GenericParam, WhereClause, Type, Expr, Error, Token, braced, bracketed,
    ExprClosure, Generics, TypeParamBound, WherePredicate, PredicateType,
    parse_quote
};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;

use quote::{quote, quote_spanned};

struct GenericParamInput(Vec<GenericParam>);
impl Parse for GenericParamInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let inner;
        bracketed!(inner in input);
        let res = inner
            .parse_terminated::<_, Token![,]>(GenericParam::parse)?;
        Ok(GenericParamInput(res.into_iter().collect()))
    }
}

fn empty_clause() -> WhereClause {
    WhereClause {
        predicates: Default::default(),
        where_token: Default::default()
    }
}

pub struct MacroInput {
    /// The target type we are implementing
    ///
    /// This has unconstrained use of the parameters defined in `params`
    target_type: Type,
    /// The generic parameters (both types and lifetimes) that we want to
    /// declare for each implementation
    ///
    /// This must not conflict with our internal names ;)
    params: Vec<GenericParam>,
    /// Custom bounds provided for each
    ///
    /// All of these bounds are optional.
    /// This option can be omitted,
    /// giving the equivalent of `bounds = {}`
    bounds: CustomBounds,
    /// The standard arguments to the macro
    options: StandardOptions,
    visit: VisitImpl
}
impl MacroInput {
    fn basic_generics(&self) -> Generics {
        let mut generics = Generics::default();
        generics.params.extend(self.params.iter().cloned());
        generics
    }
    pub fn expand_output(&self) -> Result<TokenStream, Error> {
        let target_type = &self.target_type;
        let trace_impl = self.expand_trace_impl(true)
            .expect("Trace impl required");
        let trace_immutable_impl = self.expand_trace_impl(false)?
            .unwrap_or_default();
        let gcsafe_impl = self.expand_gcsafe_impl();
        let null_trace_clause = match self.options.null_trace {
            TraitRequirements::Always => Some(empty_clause()),
            TraitRequirements::Where(ref clause) => Some(clause.clone()),
            TraitRequirements::Never => None
        };
        let null_trace_impl = if let Some(null_trace_clause) = null_trace_clause {
            let mut generics = self.basic_generics();
            generics.make_where_clause().predicates.extend(null_trace_clause.predicates.clone());
            let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
            quote! {
                unsafe impl #impl_generics NullTrace for #target_type #ty_generics
                    #where_clause {}
            }
        } else {
            quote!()
        };
        let rebrand_impl = self.expand_brand_impl(true);
        let erase_impl = self.expand_brand_impl(false);
        Ok(quote! {
            #trace_impl
            #trace_immutable_impl
            #null_trace_impl
            #gcsafe_impl
            #rebrand_impl
            #erase_impl
        })
    }
    fn expand_trace_impl(&self, mutable: bool) -> Result<Option<TokenStream>, Error> {
        let target_type = &self.target_type;
        let mut generics = self.basic_generics();
        let clause = if mutable {
            self.bounds.trace_where_clause(&self.params)
        } else {
            match self.bounds.trace_immutable_clause(&self.params) {
                Some(clause) => clause,
                None => return Ok(None), // They are requesting that we dont implement
            }
        };
        generics.make_where_clause().predicates
            .extend(clause.predicates);
        let visit_impl = self.visit.expand_impl(mutable)?;
        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
        let trait_name = if mutable { quote!(Trace) } else { quote!(TraceImmutable) };
        let visit_method_name = if mutable { quote!(visit) } else { quote!(visit_immutable) };
        Ok(Some(quote! {
            unsafe impl #impl_generics #trait_name for #target_type #ty_generics #where_clause {
                #[inline] // TODO: Should this be unconditional?
                fn #visit_method_name<V: GcVisitor + ?Sized>(&mut self, visitor: &mut V) -> Result<(), V::Err> {
                    #visit_impl
                }
            }
        }))
    }
    fn expand_gcsafe_impl(&self) -> Option<TokenStream> {
        let target_type = &self.target_type;
        let mut generics = self.basic_generics();
        generics.make_where_clause().predicates
            .extend(match self.bounds.gcsafe_clause(&self.params) {
                Some(clause) => clause.predicates,
                None => return None // They are requesting we dont implement
            });
        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
        Some(quote! {
            unsafe impl #impl_generics GcSafe for #target_type #ty_generics #where_clause {}
        })
    }
    fn expand_brand_impl(&self, rebrand: bool /* true => rebrand, false => erase */) -> Option<TokenStream> {
        let requirements = if rebrand { self.bounds.rebrand.clone() } else { self.bounds.erase.clone() };
        if let Some(TraitRequirements::Never) = requirements  {
            // They are requesting that we dont implement
            return None;
        }
        let target_type = &self.target_type;
        let mut generics = self.basic_generics();
        for param in &self.params {
            match param {
                GenericParam::Type(ref tp) => {
                    let type_name = &tp.ident;
                    generics.make_where_clause()
                        .predicates.push(WherePredicate::Type(PredicateType {
                        lifetimes: None,
                        bounded_ty: if rebrand {
                            parse_quote!(<#type_name as GcRebrand<'new_gc, Id>>::Branded)
                        } else {
                            parse_quote!(<#type_name as GcErase<'new_gc, Id>>::Erased)
                        },
                        colon_token: Default::default(),
                        bounds: tp.bounds.clone()
                    }))
                }
                _ => {}
            }
        }
        generics.make_where_clause().predicates
            .extend(create_clause_with_default(
                &requirements, &self.params,
                if rebrand {
                    vec![parse_quote!(GcRebrand<'new_gc, Id>)]
                } else {
                    vec![parse_quote!(GcErase<'min, Id>)]
                }
            ).unwrap_or_else(empty_clause).predicates);
        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
        Some(quote! {
            unsafe impl #impl_generics GcSafe for #target_type #ty_generics #where_clause {}
        })
    }
}

macro_rules! __full_field_ty {
    ($field_type:ty, opt) => (Option<$field_type>);
    ($field_type:ty,) => ($field_type);
}
macro_rules! __unwrap_field_ty {
    ($opt_name:literal, $span:expr, $val:ident, $field_ty:ty, opt) => ($val);
    ($opt_name:literal, $span:expr, $val:ident, $field_ty:ty,) => (match $val {
        Some(inner) => inner,
        None => return Err(Error::new(
            $span, concat!("Missing required option: ", $opt_name)
        ))
    });
}

macro_rules! parse_field_opts {
    ($parser:ident, {
        $($opt_name:literal [$field_type:ty] $($suffix:ident)? => $field_name:ident;)*
    }) => (parse_field_opts!($parser, complete = true, {
        $($opt_name [$field_type] $($suffix)? => $field_name;)*
    }));
    ($parser:ident, complete = $complete:literal, {
        $($opt_name:literal [$field_type:ty] $($suffix:ident)? => $field_name:ident;)*
    }) => {{
        assert!($complete, "Incomplte is unsupported!"); // NOTE: How would we parse an unknown type?
        struct ParsedFields {
            $($field_name: __full_field_ty!($field_type, $($suffix)?)),*
        }
        $(let mut $field_name = Option::<$field_type>::None;)*
        #[allow(unused)]
        let start_span = $parser.span();
        while !$parser.is_empty() {
            let ident = $parser.parse::<Ident>()
                .map_err(|e| Error::new(e.span(), "Expected an option name"))?;
            let s = ident.to_string();
            match &*s {
                $($opt_name => {
                    if $field_name.is_some() {
                        return Err(Error::new(
                            ident.span(),
                            concat!("Duplicate values specified for option: ", $opt_name),
                        ))
                    }
                    $parser.parse::<Token![=>]>()?;
                    $field_name = Some($parser.parse::<$field_type>()?);
                    if $parser.peek(Token![,]) {
                        $parser.parse::<Token![,]>()?;
                    }
                },)*
                _ => {
                    return Err(Error::new(
                        ident.span(),
                        format!("Unknown option name: {}", ident)
                    ))
                }
            }
        }
        ParsedFields {
            $($field_name: __unwrap_field_ty!($opt_name, start_span, $field_name, $field_type, $($suffix)?)),*
        }
    }};
}
impl Parse for MacroInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let res = parse_field_opts!(input, {
            "target" [Type] => target_type;
            "params" [GenericParamInput] => params;
            "bounds" [CustomBounds] opt => bounds;
            // StandardOptions
            "null_trace" [TraitRequirements] => null_trace;
            "branded" [Type] opt => branded;
            "erased" [Type] opt => erased;
            "NEEDS_TRACE" [Expr] => needs_trace;
            "NEEDS_DROP" [Expr] => needs_drop;
            "visit" [VisitClosure] opt => visit_closure;
            "trace_mut" [VisitClosure] opt => trace_mut_closure;
            "trace_immutable" [VisitClosure] opt => trace_immutable_closure;
        });
        let bounds = res.bounds.unwrap_or_default();
        if let Some(TraitRequirements::Never) = bounds.trace  {
            return Err(Error::new(
                res.target_type.span(), "Bounds on `Trace` can't be never"
            ))
        }
        let visit_impl = if let Some(visit_closure) = res.visit_closure {
            if let Some(closure) = res.trace_immutable_closure.as_ref()
                .or(res.trace_mut_closure.as_ref()) {
                return Err(Error::new(
                    closure.0.span(),
                    "Cannot specifiy specific closure (trace_mut/trace_immutable) in addition to `visit`"
                ))
            }
            VisitImpl::Generic { generic_impl: visit_closure.0 }
        } else {
            let trace_closure = res.trace_mut_closure.ok_or_else(|| {
                Error::new(
                    input.span(),
                    "Either a `visit` or a `trace_mut` impl is required for Trace types"
                )
            })?;
            let trace_immut_closure = match bounds.trace_immutable {
                Some(TraitRequirements::Never) => {
                    if let Some(closure) = res.trace_immutable_closure {
                        return Err(Error::new(
                            closure.0.span(),
                            "Specified a `trace_immutable` implementation even though TraceImmutable is never implemented"
                        ))
                    } else {
                        None
                    }
                },
                _ => {
                    let target_span = res.target_type.span();
                    // we maybe implement `TraceImmutable` some of the time
                    Some(res.trace_immutable_closure.ok_or_else(|| {
                        Error::new(
                            target_span,
                            "Requires a `trace_immutable` implementation"
                        )
                    })?)
                }
            };
            VisitImpl::Specific { mutable: trace_closure.0, immutable: trace_immut_closure.map(|closure| closure.0) }
        };
        Ok(MacroInput {
            target_type: res.target_type,
            params: res.params.0,
            bounds,
            options: StandardOptions {
                null_trace: res.null_trace,
                branded: res.branded,
                erased: res.erased,
                needs_trace: res.needs_trace,
                needs_drop: res.needs_drop
            },
            visit: visit_impl
        })
    }
}

pub struct VisitClosure(Expr);
impl Parse for VisitClosure {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let closure = match input.parse::<Expr>()? {
            Expr::Closure(closure) => closure,
            e => return Err(Error::new(e.span(), "Expected a closure"))
        };
        let ExprClosure {
            ref attrs,
            ref asyncness,
            ref movability,
            ref capture,
            or1_token: _,
            ref inputs,
            or2_token: _,
            ref output,
            ref body, ..
        } = closure;
        if !attrs.is_empty() {
            return Err(Error::new(
                closure.span(),
                "Attributes are forbidden on visit closure"
            ));
        }
        if asyncness.is_some() || movability.is_some() || capture.is_some() {
            return Err(Error::new(
                closure.span(),
                "Modifiers are forbidden on visit closure"
            ));
        }
        if let ::syn::ReturnType::Default = output {
            return Err(Error::new(
                output.span(),
                "Explicit return types are forbidden on visit closures"
            ))
        }
        if inputs.len() != 2 {
            return Err(Error::new(
                inputs.span(),
                "Expected exactly two arguments to visit closure"
            ))
        }
        fn check_input(pat: &::syn::Pat, expected_name: &str,) -> Result<(), Error> {
            match pat {
                ::syn::Pat::Ident(
                    ::syn::PatIdent {
                        ref attrs,
                        ref by_ref,
                        ref mutability,
                        ref ident,
                        ref subpat
                    }
                ) => {
                    if ident != expected_name {
                        return Err(Error::new(
                            ident.span(), format!(
                                "Expected argument `{}` for closure",
                                expected_name
                            )
                        ))
                    }
                    if !attrs.is_empty() {
                        return Err(Error::new(
                            pat.span(),
                            format!(
                                "Attributes are forbidden on closure arg `{}`",
                                expected_name
                            )
                        ))
                    }
                    if let Some(by) = by_ref {
                        return Err(Error::new(
                            by.span(),
                            format!(
                                "Explicit mutability is forbidden on closure arg `{}`",
                                expected_name
                            )
                        ))
                    }
                    if let Some(mutability) = mutability {
                        return Err(Error::new(
                            mutability.span(),
                            format!(
                                "Explicit mutability is forbidden on closure arg `{}`",
                                expected_name
                            )
                        ))
                    }
                    if let Some((_, subpat)) = subpat {
                        return Err(Error::new(
                            subpat.span(),
                            format!(
                                "Subpatterns are forbidden on closure arg `{}`",
                                expected_name
                            )
                        ))
                    }
                    assert_eq!(ident, expected_name);
                    return Ok(())
                },
                _ => {},
            }
            Err(Error::new(pat.span(), format!(
                "Expected argument {} to closure",
                expected_name
            )))
        }
        check_input(&inputs[0], "self")?;
        check_input(&inputs[1], "visitor")?;
        Ok(VisitClosure((**body).clone()))
    }
}

/// Extra bounds
#[derive(Default)]
pub struct CustomBounds {
    /// Additional bounds on the `Trace` implementation
    trace: Option<TraitRequirements>,
    /// Additional bounds on the `TraceImmutable` implementation
    ///
    /// If unspecified, this will default to the same as those
    /// specified for `Trace`
    trace_immutable: Option<TraitRequirements>,
    /// Additional bounds on the `GcSafe` implementation
    ///
    /// If unspecified, this will default to the same as those
    /// specified for `Trace`
    gcsafe: Option<TraitRequirements>,
    /// The requirements to implement `GcRebrand`
    rebrand: Option<TraitRequirements>,
    /// The requirements to implement `GcErase`
    erase: Option<TraitRequirements>,
}
impl CustomBounds {
    fn trace_where_clause(&self, generic_params: &[GenericParam]) -> WhereClause {
        create_clause_with_default(
            &self.trace, generic_params,
            vec![parse_quote!(Trace)]
        ).unwrap_or_else(|| unreachable!("Trace must always be implemented"))
    }
    fn trace_immutable_clause(&self, generic_params: &[GenericParam]) -> Option<WhereClause> {
        create_clause_with_default(
            &self.trace_immutable, generic_params,
            vec![parse_quote!(TraceImmutable)]
        )
    }
    fn gcsafe_clause(&self, generic_params: &[GenericParam]) -> Option<WhereClause> {
        create_clause_with_default(
            &self.gcsafe, generic_params,
            vec![parse_quote!(GcSafe)]
        )
    }
    fn rebrand_clause(&self, generic_params: &[GenericParam]) -> Option<WhereClause> {
        create_clause_with_default(
            &self.rebrand, generic_params,
            vec![parse_quote!(GcRebrand<'new_gc, Id>)]
        )
    }
    fn erase_clause(&self, generic_params: &[GenericParam]) -> Option<WhereClause> {
        create_clause_with_default(
            &self.erase, generic_params,
            vec![parse_quote!(GcErase<'min, Id>)]
        )
    }
}
fn create_clause_with_default(
    target: &Option<TraitRequirements>, generic_params: &[GenericParam],
    default_bounds: Vec<TypeParamBound>
) -> Option<WhereClause> {
    Some(match *target {
        Some(TraitRequirements::Never) => return None, // do not implement
        Some(TraitRequirements::Where(ref explicit)) => explicit.clone(),
        Some(TraitRequirements::Always) => {
            // Absolutely no conditions on implementation
            empty_clause()
        }
        None => {
            let mut where_clause = empty_clause();
            // Infer bounds for all params
            for param in generic_params {
                if let GenericParam::Type(ref t) = param {
                    let ident = &t.ident;
                    where_clause.predicates.push(WherePredicate::Type(PredicateType {
                        bounded_ty: parse_quote!(#ident),
                        colon_token: Default::default(),
                        bounds: default_bounds.iter().cloned().collect(),
                        lifetimes: None
                    }))
                }
            }
            let type_idents = generic_params.iter()
                .filter_map(|param| match param {
                    GenericParam::Type(ref t) => {
                        Some(t.ident.clone())
                    },
                    _ => None
                }).collect::<Vec<_>>();
            parse_quote!(where #(#type_idents: Trace)*)
        }
    })
}
impl Parse for CustomBounds {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let inner;
        braced!(inner in input);
        let res = parse_field_opts!(inner, {
            "Trace" [TraitRequirements] opt => trace;
            "TraceImmutable" [TraitRequirements] opt => trace_immutable;
            "GcSafe" [TraitRequirements] opt => gcsafe;
            "GcRebrand" [TraitRequirements] opt => rebrand;
            "GcErase" [TraitRequirements] opt => erase;
        });
        Ok(CustomBounds {
            trace: res.trace,
            trace_immutable: res.trace_immutable,
            gcsafe: res.gcsafe,
            rebrand: res.rebrand,
            erase: res.erase
        })
    }
}

/// The standard options for the macro
///
/// Options are required unless wrapped in an `Option`
/// (or explicitly marked optional)
pub struct StandardOptions {
    /// Requirements on implementing the NullTrace
    ///
    /// This is unsafe, and completely unchecked.
    null_trace: TraitRequirements,
    /// The associated type implemented as `GcRebrand::Branded`
    branded: Option<Type>,
    /// The associated type implemented as `GcErase::Erased`
    erased: Option<Type>,
    /// A (constant) expression determining whether the array needs to be traced
    needs_trace: Expr,
    /// A (constant) expression determining whether the type should be dropped
    needs_drop: Expr,
}

/// The visit implementation.
///
/// The target object is always accessible through `self`.
/// Other variables depend on the implementation.
pub enum VisitImpl {
    /// A generic implementation, whose code is shared across
    /// both mutable/immutable implementations.
    ///
    /// This requires auto-replacement of certain magic variables,
    /// which vary depending on whether we're generating a mutable
    /// or immutable visitor.
    ///
    /// There are two variables accessible to the implementation: `self` and `visitor`
    ///
    /// | Magic Variable | for Trace  | for TraceImmutable |
    /// | -------------- | ---------- | ------------------ |
    /// | #mutability    | `` (empty) | `mut`              |
    /// | #as_ref        | `as_ref`   | `as_mut`           |
    /// | #iter          | `iter`     | `iter_mut`         |
    /// | #visit_func    | `visit`    | `visit_immutable`  |
    /// | #b             | `&`        | `&mut`             |
    /// | ## (escape)    | `#`        | `#`                |
    ///
    /// Example visitor for `Vec<T>`:
    /// ````no_test
    /// for item in self.#iter() {
    ///     #visit(item);
    /// }
    /// Ok(())
    /// ````
    Generic {
        generic_impl: Expr
    },
    /// Specialized implementations which are different for
    /// both `Trace` and `TraceImmutable`
    Specific {
        mutable: Expr,
        immutable: Option<Expr>
    }
}
enum MagicVarType {
    Mutability,
    AsRef,
    Iter,
    VisitFunc,
    B
}
impl MagicVarType {
    fn parse_ident(ident: &Ident) -> Result<MagicVarType, Error> {
        let s = ident.to_string();
        Ok(match &*s {
            "mutability" => MagicVarType::Mutability,
            "as_ref" => MagicVarType::AsRef,
            "iter" => MagicVarType::Iter,
            "visit_func" => MagicVarType::VisitFunc,
            "b" => MagicVarType::B,
            _ => return Err(Error::new(ident.span(), "Invalid magic variable name"))
        })
    }
}
impl VisitImpl {
    fn expand_impl(&self, mutable: bool) -> Result<Expr, Error> {
        use quote::ToTokens;
        match *self {
            VisitImpl::Generic { ref generic_impl } => {
                ::syn::parse2(replace_magic_tokens(generic_impl.to_token_stream(), &mut |ident| {
                    let res = match MagicVarType::parse_ident(ident)? {
                        MagicVarType::Mutability => {
                            if mutable {
                                quote!(mut)
                            } else {
                                quote!()
                            }
                        }
                        MagicVarType::AsRef => {
                            if mutable {
                                quote!(as_mut)
                            } else {
                                quote!(as_ref)
                            }
                        }
                        MagicVarType::Iter => {
                            if mutable {
                                quote!(iter_mut)
                            } else {
                                quote!(iter)
                            }
                        }
                        MagicVarType::VisitFunc => {
                            if mutable {
                                quote!(visit)
                            } else {
                                quote!(visit_immutable)
                            }
                        }
                        MagicVarType::B => {
                            if mutable {
                                quote!(&mut)
                            } else {
                                quote!()
                            }
                        }
                    };
                    let span = ident.span(); // Reuse the span of the *input*
                    Ok(quote_spanned!(span => #res))
                })?)
            }
            VisitImpl::Specific { mutable: ref mutable_impl, ref immutable } => {
                Ok(if mutable {
                    mutable_impl.clone()
                } else {
                    immutable.clone().unwrap()
                })
            }
        }
    }
}
fn replace_magic_tokens(input: TokenStream, func: &mut dyn FnMut(&Ident) -> Result<TokenStream, Error>) -> Result<TokenStream, Error> {
    use quote::TokenStreamExt;
    let mut res = TokenStream::new();
    let mut iter = input.into_iter();
    while let Some(item) = iter.next() {
        match item {
            TokenTree::Group(ref group) => {
                let old_span = group.span();
                let delim = group.delimiter();
                let new_stream = replace_magic_tokens(group.stream(), &mut *func)?;
                let mut new_group = ::proc_macro2::Group::new(delim, new_stream);
                new_group.set_span(old_span); // The overall span must be preserved
                res.append(TokenTree::Group(new_group))
            }
            TokenTree::Punct(ref p) if p.as_char() == '#' => {
                match iter.next() {
                    None => return Err(Error::new(
                        p.span(), "Unexpected EOF after magic token `#`"
                    )),
                    Some(TokenTree::Punct(ref p2)) if p2.as_char() == '#' => {
                        // Pass through p2
                        res.append(TokenTree::Punct(p2.clone()));
                    }
                    Some(TokenTree::Ident(ref ident)) => {
                        res.extend(func(ident)?);
                    },
                    Some(ref other) => {
                        return Err(Error::new(
                            p.span(), format!(
                                "Invalid token after magic token `#`: {}",
                                other
                            )
                        ))
                    }
                }
            }
            TokenTree::Punct(_) | TokenTree::Ident(_) | TokenTree::Literal(_)=> {
                // pass through
                res.append(item);
            }
        }
    }
    Ok(res)
}

/// The requirements to implement a trait
///
/// In addition to a where clause, you can specify `always` for unconditional
/// implementation and `never` to forbid generated implementations
#[derive(Clone)]
pub enum TraitRequirements {
    /// The trait should never be implemented
    Never,
    /// The trait should only be implemented if
    /// the specified where clause is satisfied
    Where(WhereClause),
    /// The trait should always be implemented
    Always
}

impl Parse for TraitRequirements {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(syn::Ident) {
            let ident = input.parse::<Ident>()?;
            if ident == "always" {
                Ok(TraitRequirements::Always)
            } else if ident == "never" {
                Ok(TraitRequirements::Never)
            } else {
                return Err(Error::new(
                    ident.span(),
                    "Invalid identifier for `TraitRequirement`"
                ))
            }
        } else if input.peek(syn::token::Brace) {
            let inner;
            braced!(inner in input);
            Ok(TraitRequirements::Where(inner.parse::<WhereClause>()?))
        } else {
            return Err(input.error("Invalid `TraitRequirement`"))
        }
    }
}