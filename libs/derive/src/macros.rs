//! Procedural macros for implementing `GcType`

/*
 * The main macro here is `unsafe_impl_gc`
 */

use std::collections::HashSet;

use proc_macro2::{Ident, TokenStream, TokenTree};
use syn::{
    GenericParam, WhereClause, Type, Expr, Error, Token, braced, bracketed,
    Generics, TypeParamBound, WherePredicate, PredicateType, parse_quote,
    GenericArgument
};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;

use quote::{quote, quote_spanned};
use super::zerogc_crate;

#[derive(Debug)]
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

#[derive(Debug)]
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
        let zerogc_crate = zerogc_crate();
        let target_type = &self.target_type;
        let trace_impl = self.expand_trace_impl(true)?
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
            let (impl_generics, _, where_clause) = generics.split_for_impl();
            quote! {
                unsafe impl #impl_generics #zerogc_crate::NullTrace for #target_type
                    #where_clause {}
            }
        } else {
            quote!()
        };
        let rebrand_impl = self.expand_brand_impl(true)?;
        let erase_impl = self.expand_brand_impl(false)?;
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
        let zerogc_crate = zerogc_crate();
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
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        let trait_name = if mutable { quote!(#zerogc_crate::Trace) } else { quote!(#zerogc_crate::TraceImmutable) };
        let visit_method_name = if mutable { quote!(visit) } else { quote!(visit_immutable) };
        let needs_trace_const = if mutable {
            let expr = &self.options.needs_trace;
            Some(quote!(const NEEDS_TRACE: bool = {
                // Import the trait so we can access `T::NEEDS_TRACE`
                use #zerogc_crate::Trace;
                #expr
            };))
        } else {
            None
        };
        let mutability = if mutable {
            quote!(mut)
        } else {
            quote!()
        };
        Ok(Some(quote! {
            unsafe impl #impl_generics #trait_name for #target_type #where_clause {
                #needs_trace_const
                #[inline] // TODO: Should this be unconditional?
                fn #visit_method_name<Visitor: #zerogc_crate::GcVisitor + ?Sized>(&#mutability self, visitor: &mut Visitor) -> Result<(), Visitor::Err> {
                    #visit_impl
                }
            }
        }))
    }
    fn expand_gcsafe_impl(&self) -> Option<TokenStream> {
        let zerogc_crate = zerogc_crate();
        let target_type = &self.target_type;
        let mut generics = self.basic_generics();
        generics.make_where_clause().predicates
            .extend(match self.bounds.gcsafe_clause(&self.params) {
                Some(clause) => clause.predicates,
                None => return None // They are requesting we dont implement
            });
        let needs_drop = &self.options.needs_drop;
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        Some(quote! {
            unsafe impl #impl_generics #zerogc_crate::GcSafe for #target_type #where_clause {
                const NEEDS_DROP: bool = {
                    // Import the trait so we can access `T::NEEDS_DROP`
                    use #zerogc_crate::GcSafe;
                    #needs_drop
                };
            }
        })
    }
    fn expand_brand_impl(&self, rebrand: bool /* true => rebrand, false => erase */) -> Result<Option<TokenStream>, Error> {
        let zerogc_crate = zerogc_crate();
        let requirements = if rebrand { self.bounds.rebrand.clone() } else { self.bounds.erase.clone() };
        if let Some(TraitRequirements::Never) = requirements  {
            // They are requesting that we dont implement
            return Ok(None);
        }
        let target_type = &self.target_type;
        let mut generics = self.basic_generics();
        let id_type: Type = match self.options.collector_id {
            Some(ref tp) => tp.clone(),
            None => {
                generics.params.push(parse_quote!(Id: #zerogc_crate::CollectorId));
                parse_quote!(Id)
            }
        };
        let default_bounds: Vec<TypeParamBound> = match requirements {
            Some(TraitRequirements::Where(ref explicit_requirements)) => {
                generics.make_where_clause().predicates
                    .extend(explicit_requirements.predicates.iter().cloned());
                // they have explicit requirements -> no default bounds
                vec![]
            }
            Some(TraitRequirements::Always) => {
                vec![] // always should implement
            },
            Some(TraitRequirements::Never) => unreachable!(),
            None => {
                if rebrand {
                    vec![parse_quote!(#zerogc_crate::GcRebrand<'new_gc, #id_type>)]
                } else {
                    vec![parse_quote!(#zerogc_crate::GcErase<'min, #id_type>)]
                }
            }
        };
        // generate default bounds
        for param in &self.params {
            if default_bounds.is_empty() {
                // no defaults to generate
                break
            }
            match param {
                GenericParam::Type(ref tp) => {
                    let type_name = &tp.ident;
                    let mut bounds = tp.bounds.clone();
                    bounds.extend(default_bounds.iter().cloned());
                    generics.make_where_clause()
                        .predicates.push(WherePredicate::Type(PredicateType {
                        lifetimes: None,
                        bounded_ty: if rebrand {
                            self.options.branded_type.clone().unwrap_or_else(|| {
                                parse_quote!(<#type_name as #zerogc_crate::GcRebrand<'new_gc, Id>>::Branded)
                            })
                        } else {
                            self.options.erased_type.clone().unwrap_or_else(|| {
                                parse_quote!(<#type_name as #zerogc_crate::GcErase<'min, Id>>::Erased)
                            })
                        },
                        colon_token: Default::default(),
                        bounds: bounds.clone(),
                    }));
                    generics.make_where_clause()
                        .predicates.push(WherePredicate::Type(PredicateType {
                        lifetimes: None,
                        bounded_ty: parse_quote!(#type_name),
                        colon_token: Default::default(),
                        bounds
                    }))
                }
                _ => {}
            }
        }
        /*
         * If we don't have explicit specification,
         * extend the with the trace clauses
         *
         * TODO: Do we need to apply to the `Branded`/`Erased` types
         */
        generics.make_where_clause().predicates
            .extend(self.bounds.trace_where_clause(&self.params).predicates);
        if rebrand {
            generics.params.push(parse_quote!('new_gc));
        } else {
            generics.params.push(parse_quote!('min));
        }
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        let target_trait = if rebrand {
            quote!(#zerogc_crate::GcRebrand<'new_gc, #id_type>)
        } else {
            quote!(#zerogc_crate::GcErase<'min, #id_type>)
        };
        fn rewrite_brand_trait(
            target: &Type, trait_name: &str, target_params: &HashSet<Ident>,
            target_trait: TokenStream, associated_type: Ident
        ) -> Result<Type, Error> {
            rewrite_type(target, trait_name, &mut |target_type| {
                let ident = match target_type {
                    Type::Path(ref tp) if tp.qself.is_none() => {
                        match tp.path.get_ident() {
                            Some(ident) => ident,
                            None => return None
                        }
                    },
                    _ => return None
                };
                if target_params.contains(&ident) {
                    Some(parse_quote!(<#ident as #target_trait>::#associated_type))
                } else {
                    None
                }
            })
        }
        let target_params = self.params.iter().filter_map(|param| match param {
            GenericParam::Type(ref tp) => Some(tp.ident.clone()),
            _ => None
        }).collect::<HashSet<_>>();
        let associated_type = if rebrand {
            let branded = self.options.branded_type.clone().map_or_else(|| {
                rewrite_brand_trait(
                    &self.target_type, "GcRebrand",
                    &target_params,
                    parse_quote!(#zerogc_crate::GcRebrand<'new_gc, #id_type>),
                    parse_quote!(Branded)
                )
            }, Ok)?;
            quote!(type Branded = #branded;)
        } else {
            let erased = Ok(self.options.erased_type.clone()).transpose().unwrap_or_else(|| {
                rewrite_brand_trait(
                    &self.target_type, "GcErase",
                    &target_params,
                    parse_quote!(#zerogc_crate::GcErase<'min, #id_type>),
                    parse_quote!(Erased)
                )
            })?;
            quote!(type Erased = #erased;)
        };
        Ok(Some(quote! {
            unsafe impl #impl_generics #target_trait for #target_type #where_clause {
                #associated_type
            }
        }))
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
            "branded_type" [Type] opt => branded_type;
            "erased_type" [Type] opt => erased_type;
            "NEEDS_TRACE" [Expr] => needs_trace;
            "NEEDS_DROP" [Expr] => needs_drop;
            "visit" [VisitClosure] opt => visit_closure;
            "trace_mut" [VisitClosure] opt => trace_mut_closure;
            "trace_immutable" [VisitClosure] opt => trace_immutable_closure;
            "collector_id" [Type] opt => collector_id;
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
                    closure.body.span(),
                    "Cannot specify specific closure (trace_mut/trace_immutable) in addition to `visit`"
                ))
            }
            VisitImpl::Generic { generic_impl: visit_closure.body }
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
                            closure.body.span(),
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
            VisitImpl::Specific {
                mutable: ::syn::parse2(trace_closure.body)?,
                immutable: trace_immut_closure
                    .map(|closure| ::syn::parse2::<Expr>(closure.body))
                    .transpose()?
            }
        };
        Ok(MacroInput {
            target_type: res.target_type,
            params: res.params.0,
            bounds,
            options: StandardOptions {
                null_trace: res.null_trace,
                branded_type: res.branded_type,
                erased_type: res.erased_type,
                needs_trace: res.needs_trace,
                needs_drop: res.needs_drop,
                collector_id: res.collector_id
            },
            visit: visit_impl
        })
    }
}

#[derive(Debug)]
pub struct VisitClosure {
    body: TokenStream,
    brace: ::syn::token::Brace
}
impl Parse for VisitClosure {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<Token![|]>()?;
        if !input.peek(Token![self]) {
            return Err(Error::new(
                input.span(),
                "Expected first argument to closure to be `self`"
            ));
        }
        input.parse::<Token![self]>()?;
        input.parse::<Token![,]>()?;
        let visitor_name = input.parse::<Ident>()?;
        if visitor_name != "visitor" {
            return Err(Error::new(
                visitor_name.span(),
                "Expected second argument to closure to be `visitor`"
            ));
        }
        input.parse::<Token![|]>()?;
        if !input.peek(syn::token::Brace) {
            return Err(input.error("Expected visitor closure to be braced"));
        }
        let body;
        let brace = braced!(body in input);
        let body = body.parse::<TokenStream>()?;
        Ok(VisitClosure { body: quote!({ #body }), brace })
    }
}

/// Extra bounds
#[derive(Default, Debug)]
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
        let zerogc_crate = zerogc_crate();
        create_clause_with_default(
            &self.trace, generic_params,
            vec![parse_quote!(#zerogc_crate::Trace)]
        ).unwrap_or_else(|| unreachable!("Trace must always be implemented"))
    }
    fn trace_immutable_clause(&self, generic_params: &[GenericParam]) -> Option<WhereClause> {
        let zerogc_crate = zerogc_crate();
        create_clause_with_default(
            &self.trace_immutable, generic_params,
            vec![parse_quote!(#zerogc_crate::TraceImmutable)]
        )
    }
    fn gcsafe_clause(&self, generic_params: &[GenericParam]) -> Option<WhereClause> {
        let zerogc_crate = zerogc_crate();
        let mut res = create_clause_with_default(
            &self.gcsafe, generic_params,
            vec![parse_quote!(#zerogc_crate::GcSafe)]
        );
        if self.gcsafe.is_none() {
            // Extend with the trae bounds
            res.get_or_insert_with(empty_clause).predicates.extend(
                self.trace_where_clause(generic_params).predicates
            )
        }
        res
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
            where_clause
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
            erase: res.erase,
        })
    }
}

/// The standard options for the macro
///
/// Options are required unless wrapped in an `Option`
/// (or explicitly marked optional)
#[derive(Debug)]
pub struct StandardOptions {
    /// Requirements on implementing the NullTrace
    ///
    /// This is unsafe, and completely unchecked.
    null_trace: TraitRequirements,
    /// The associated type implemented as `GcRebrand::Branded`
    branded_type: Option<Type>,
    /// The associated type implemented as `GcErase::Erased`
    erased_type: Option<Type>,
    /// A (constant) expression determining whether the array needs to be traced
    needs_trace: Expr,
    /// A (constant) expression determining whether the type should be dropped
    needs_drop: Expr,
    /// The fixed id of the collector, or `None` if the type can work with any collector
    collector_id: Option<Type>
}

/// The visit implementation.
///
/// The target object is always accessible through `self`.
/// Other variables depend on the implementation.
#[derive(Debug)]
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
        generic_impl: TokenStream
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
            _ => return Err(
                Error::new(ident.span(),
                           "Invalid magic variable name"
                ))
        })
    }
}
impl VisitImpl {
    fn expand_impl(&self, mutable: bool) -> Result<Expr, Error> {
        match *self {
            VisitImpl::Generic { ref generic_impl } => {
                let tokens = replace_magic_tokens(generic_impl.clone(), &mut |ident| {
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
                                quote!(&)
                            }
                        }
                    };
                    let span = ident.span(); // Reuse the span of the *input*
                    Ok(quote_spanned!(span => #res))
                })?;
                Ok(match ::syn::parse2::<Expr>(tokens.clone()) {
                    Ok(res) => res,
                    Err(cause) => {
                        let mut err = Error::new(
                            generic_impl.span(),
                            format!(
                                "Unable to perform 'magic' variable substitution on closure: {}",
                                tokens
                            )
                        );
                        err.combine(cause);
                        return Err(err)
                    }
                })
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
#[derive(Clone, Debug)]
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



fn rewrite_type(target: &Type, target_type_name: &str, rewriter: &mut dyn FnMut(&Type) -> Option<Type>) -> Result<Type, Error> {
    if let Some(explicitly_rewritten) = rewriter(target) {
        return Ok(explicitly_rewritten)
    }
    let mut target = target.clone();
    match target {
        Type::Paren(ref mut inner) => {
            *inner.elem = rewrite_type(&inner.elem, target_type_name, rewriter)?
        },
        Type::Group(ref mut inner) => {
            *inner.elem = rewrite_type(&inner.elem, target_type_name, rewriter)?
        },
        Type::Reference(ref mut target) => {
            // TODO: Lifetime safety?
            // Rewrite reference target
            *target.elem = rewrite_type(&target.elem, target_type_name, rewriter)?
        }
        Type::Path(::syn::TypePath { ref mut qself, ref mut path }) => {
            *qself = qself.clone().map::<Result<_, Error>, _>(|mut qself| {
                qself.ty = Box::new(rewrite_type(
                    &*qself.ty, target_type_name,
                    &mut *rewriter
                )?);
                Ok(qself)
            }).transpose()?;
            path.segments = path.segments.iter().cloned().map(|mut segment| {
                // old_segment.ident is ignored...
                match segment.arguments {
                    ::syn::PathArguments::None => {}, // Nothing to do here
                    ::syn::PathArguments::AngleBracketed(ref mut generic_args) => {
                        for arg in &mut generic_args.args {
                            match arg {
                                GenericArgument::Lifetime(_) | GenericArgument::Const(_) => {},
                                GenericArgument::Type(ref mut generic_type) => {
                                    *generic_type = rewrite_type(generic_type, target_type_name, &mut *rewriter)?;
                                }
                                GenericArgument::Constraint(_) | GenericArgument::Binding(_) => {
                                    return Err(Error::new(
                                        arg.span(), format!(
                                            "Unable to handle generic arg while rewriting as a {}",
                                            target_type_name
                                        )
                                    ))
                                }
                            }
                        }
                    }
                    ::syn::PathArguments::Parenthesized(ref mut paran_args) => {
                        return Err(Error::new(
                            paran_args.span(),
                            "TODO: Rewriting paranthesized (fn-style) args"
                        ));
                    }
                }
                Ok(segment)
            }).collect::<Result<_, Error>>()?;
        }
        _ => return Err(Error::new(target.span(), format!(
            "Unable to rewrite type as a `{}`: {}",
            target_type_name, quote!(#target)
        )))
    }
    Ok(target)
}
