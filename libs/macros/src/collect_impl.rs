//! The implementation of `unsafe_collect_impl!`
//!
//! Loosely based on `zerogc_derive::unsafe_gc_impl!` version `0.2.0-alpha.7`.
//! See here for the zerogc implementation:
//! <https://github.com/DuckLogic/zerogc/blob/v0.2.0-alpha.7/libs/derive/src/macros.rs>
//!
//! A significant portion of code is copied from that file.
//! There is no copyright issue because I am the only author.
use indexmap::{indexmap, IndexMap};
use proc_macro2::{Ident, Span, TokenStream};
use proc_macro_kwargs::parse::{NestedDict, NestedList, Syn};
use proc_macro_kwargs::{MacroArg, MacroKeywordArgs};
use quote::quote;
use std::process::id;
use syn::parse::ParseStream;
use syn::{
    braced, parse_quote, Error, Expr, GenericParam, Generics, Lifetime, Path, Token, Type,
    TypeParamBound, WhereClause, WherePredicate,
};

use crate::helpers::{self, zerogc_next_crate};

fn empty_clause() -> WhereClause {
    WhereClause {
        predicates: Default::default(),
        where_token: Default::default(),
    }
}

#[derive(Debug, MacroKeywordArgs)]
pub struct MacroInput {
    /// The target type we are implementing
    ///
    /// This has unconstrained use of the parameters defined in `params`
    #[kwarg(rename = "target")]
    pub target_type: Type,
    /// The generic parameters (both types and lifetimes) that we want to
    /// declare for each implementation
    ///
    /// This must not conflict with our internal generic names ;)
    params: NestedList<GenericParam>,
    /// Custom bounds provided for each
    ///
    /// All of these bounds are optional.
    /// This option can be omitted,
    /// giving the equivalent of `bounds = {}`
    #[kwarg(optional)]
    bounds: CustomBounds,
    /// Requirements for implementing `NullCollect`
    ///
    /// This is unsafe (obviously) and has no static checking.
    null_collect: Option<TraitRequirements>,
    /// The associated type `Collect::Collected<'newgc>`
    ///
    /// Implicitly takes a `'newgc` parameter
    collected_type: Type,
    /// A (constant) expression determining whether the type needs to be collected
    #[kwarg(rename = "NEEDS_COLLECT")]
    needs_collect: Expr,
    /// The fixed `CollectorId`s the type should implement `Collect` for,
    /// or `*` if the type can work with any collector.
    ///
    /// If multiple collector ids are specified,
    /// each one must have a specific lifetime.
    #[kwarg(optional, rename = "collector_id")]
    collector_id_info: CollectorIdInfo,
    /// The actual implementation of the `Collect::copy_collect` method
    ///
    /// Because of the way the trait is designed,
    /// this is a relatively safe method.
    ///
    /// This code is implicitly skipped if `Self::NEEDS_COLLECt` is `false`
    #[kwarg(rename = "copy_collect")]
    copy_collect_closure: CollectImplClosure,
}
fn generic_collector_id_param() -> Ident {
    parse_quote!(AnyCollectorId)
}
impl MacroInput {
    fn basic_generics(&self) -> Generics {
        let mut generics = Generics::default();
        generics.params.extend(self.params.iter().cloned());
        generics
    }
    fn setup_collector_id_generics(&self, target: &mut Generics) -> syn::Result<Path> {
        match self.collector_id_info {
            CollectorIdInfo::Any => {
                let zerogc_next_crate = zerogc_next_crate();
                let collector_id_param = generic_collector_id_param();
                target.params.push(GenericParam::Type(parse_quote!(
                    #collector_id_param: #zerogc_next_crate::CollectorId
                )));
                Ok(syn::Path::from(collector_id_param))
            }
            CollectorIdInfo::Specific { ref map } => {
                let (collector_id, _lifetime) = match map.len() {
                    0 => {
                        return Err(Error::new(
                            Span::call_site(),
                            "Using 'specific' CollectorId, but none specified",
                        ));
                    }
                    1 => map.get_index(0).unwrap(),
                    _ => {
                        let mut errors = Vec::new();
                        for (pth, _) in map.iter() {
                            errors.push(Error::new_spanned(
                                pth,
                                "Multiple `CollectorId`s is currently unsupported",
                            ));
                        }
                        return Err(helpers::combine_errors(errors).unwrap_err());
                    }
                };
                Ok(collector_id.clone())
            }
        }
    }
    pub fn expand_output(&self) -> Result<TokenStream, Error> {
        let zerogc_next_crate = zerogc_next_crate();
        let target_type = &self.target_type;
        let collect_impl = self.expand_collect_impl()?;
        let null_collect_clause = match self.null_collect {
            TraitRequirements::Always { span: _ } => Some(empty_clause()),
            TraitRequirements::Where(ref clause) => Some(clause.clone()),
            TraitRequirements::Never { span: _ } => None,
        };
        let null_collect_impl: TokenStream = if let Some(null_collect_clause) = null_collect_clause
        {
            let mut generics = self.basic_generics();
            let collector_id = self.setup_collector_id_generics(&mut generics)?;
            generics
                .make_where_clause()
                .predicates
                .extend(null_collect_clause.predicates);
            let (impl_generics, _, where_clause) = generics.split_for_impl();
            quote! {
                unsafe impl #impl_generics #zerogc_next_crate::NullCollect<#collector_id> for #target_type
                    #where_clause {}
            }
        } else {
            quote!()
        };
        Ok(quote! {
            #collect_impl
            #null_collect_impl
        })
    }
    fn expand_collect_impl(&self) -> Result<TokenStream, Error> {
        let zerogc_next_crate = zerogc_next_crate();
        let target_type = &self.target_type;
        let mut generics: syn::Generics = self.basic_generics();
        let collector_id = self.setup_collector_id_generics(&mut generics);
        {
            let clause = self.bounds.where_clause_collect(&self.params.elements)?;
            generics
                .make_where_clause()
                .predicates
                .extend(clause.predicates);
        }
        // NOTE: The `expand` method implicitly skips body if `!Self::NEEDS_COLLECT`
        let collect_impl = self.copy_collect_closure.expand();
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        let needs_collect_const = {
            let expr = &self.needs_collect;
            quote!(const NEEDS_TRACE: bool = {
                // Import the trait so we can access `T::NEEDS_COLLECT`
                use #zerogc_next_crate::Collect;
                #expr
            };)
        };
        let collected_type = &self.collected_type;
        Ok(quote! {
            unsafe impl #impl_generics #zerogc_next_crate::Collect<#collector_id> for #target_type #where_clause {
                type Collected<'newgc> = #collected_type;
                #needs_collect_const

                #[inline] // TODO: Should this be unconditional?
                #[allow(dead_code)] // possible if `Self::NEEDS_COLLECT` is always false
                fn copy_collect<'newgc>(self, context: &mut #zerogc_next_crate::context::CollectContext<'newgc>) -> Self::Collected<'newgc> {
                    #collect_impl
                }
            }
        })
    }
}

/// Extra bounds
#[derive(Default, Debug, MacroKeywordArgs)]
pub struct CustomBounds {
    /// Additional bounds on the `Collect` implementation
    #[kwarg(optional, rename = "Collect")]
    collect: Option<TraitRequirements>,
}

impl CustomBounds {
    fn where_clause_collect(
        &self,
        generic_params: &[GenericParam],
    ) -> Result<WhereClause, syn::Error> {
        match self.collect {
            Some(TraitRequirements::Never { span }) => {
                return Err(syn::Error::new(span, "Collect must always be implemented"))
            }
            Some(TraitRequirements::Always) => Ok(empty_clause()), // No requirements
            Some(TraitRequirements::Where(ref explicit)) => Ok(explicit.clone()),
            None => {
                // generate the implicit requirements
                let zerogc_next_crate = zerogc_next_crate();
                Ok(create_clause_with_default(
                    &self.collect,
                    generic_params,
                    vec![parse_quote!(#zerogc_next_crate::Collect)],
                )
                .expect("Already checked for TraitRequirements::Never"))
            }
        }
    }
}

/// The requirements to implement a trait
///
/// In addition to a where clause, you can specify `always` for unconditional
/// implementation and `never` to forbid generated implementations
#[derive(Clone, Debug)]
pub enum TraitRequirements {
    /// The trait should never be implemented
    Never { span: Span },
    /// The trait should only be implemented if
    /// the specified where clause is satisfied
    Where(WhereClause),
    /// The trait should always be implemented
    Always { span: Span },
}

impl MacroArg for TraitRequirements {
    fn parse_macro_arg(input: ParseStream) -> syn::Result<Self> {
        if input.peek(syn::Ident) {
            let ident = input.parse::<Ident>()?;
            let span = ident.span();
            if ident == "always" {
                Ok(TraitRequirements::Always { span })
            } else if ident == "never" {
                Ok(TraitRequirements::Never { span })
            } else {
                Err(Error::new(
                    ident.span(),
                    "Invalid identifier for `TraitRequirement`",
                ))
            }
        } else if input.peek(syn::token::Brace) {
            let inner;
            syn::braced!(inner in input);
            Ok(TraitRequirements::Where(inner.parse::<WhereClause>()?))
        } else {
            Err(input.error("Invalid `TraitRequirement`"))
        }
    }
}

#[derive(Clone, Debug)]
pub enum CollectorIdInfo {
    Any,
    Specific { map: IndexMap<Path, Lifetime> },
}
impl Default for CollectorIdInfo {
    fn default() -> Self {
        CollectorIdInfo::Any
    }
}
impl CollectorIdInfo {
    /// Create info from a single `CollectorId`,
    /// implicitly assuming its lifetime is `'gc`
    pub fn single(path: Path) -> Self {
        CollectorIdInfo::Specific {
            map: indexmap! {
                path => parse_quote!('gc)
            },
        }
    }
}
impl MacroArg for CollectorIdInfo {
    fn parse_macro_arg(stream: ParseStream) -> syn::Result<Self> {
        if stream.peek(Token![*]) {
            stream.parse::<Token![*]>()?;
            Ok(CollectorIdInfo::Any)
        } else if stream.peek(syn::Ident) {
            Ok(CollectorIdInfo::single(stream.parse()?))
        } else if stream.peek(syn::token::Brace) {
            let inner = NestedDict::parse_macro_arg(stream)?;
            Ok(CollectorIdInfo::Specific {
                map: inner.elements,
            })
        } else {
            Err(stream.error("Expected either `*`, a path, or a map of Path => Lifetime"))
        }
    }
}

#[derive(Debug, Clone)]
pub struct KnownArgClosure {
    body: TokenStream,
    brace: ::syn::token::Brace,
}
impl KnownArgClosure {
    pub fn parse_with_fixed_args(input: ParseStream, fixed_args: &[&str]) -> syn::Result<Self> {
        let arg_start = input.parse::<Token![|]>()?.span;
        let mut actual_args = Vec::new();
        while !input.peek(Token![|]) {
            // Use 'parse_any' to accept keywords like 'self'
            actual_args.push(Ident::parse_any(input)?);
            if input.peek(Token![|]) {
                break; // done
            } else {
                input.parse::<Token![,]>()?;
            }
        }
        let arg_end = input.parse::<Token![|]>()?.span;
        if actual_args.len() != fixed_args.len() {
            return Err(Error::new(
                arg_start.join(arg_end).unwrap(),
                format!(
                    "Expected {} args but got {}",
                    fixed_args.len(),
                    actual_args.len()
                ),
            ));
        }
        for (index, (actual, &expected)) in actual_args.iter().zip(fixed_args).enumerate() {
            if *actual != expected {
                return Err(Error::new(
                    actual.span(),
                    format!("Expected arg #{} to be named {:?}", index, expected),
                ));
            }
        }
        if !input.peek(syn::token::Brace) {
            return Err(input.error("Expected visitor closure to be braced"));
        }
        let body;
        let brace = braced!(body in input);
        let body = body.parse::<TokenStream>()?;
        Ok(KnownArgClosure {
            body: quote!({ #body }),
            brace,
        })
    }
}
#[derive(Debug, Clone)]
pub struct CollectImplClosure(KnownArgClosure);
impl CollectImplClosure {
    pub fn expand(&self) -> TokenStream {
        let body = &self.0.body;
        quote! {
            if !Self::NEEDS_COLLECT {
                return context.null_copy(self);
            }
            #body
        }
    }
}
impl MacroArg for CollectImplClosure {
    fn parse_macro_arg(input: ParseStream) -> syn::Result<Self> {
        Ok(CollectImplClosure(KnownArgClosure::parse_with_fixed_args(
            input,
            &["self", "context"],
        )?))
    }
}

fn create_clause_with_default(
    target: &Option<TraitRequirements>,
    generic_params: &[GenericParam],
    default_bounds: Vec<TypeParamBound>,
) -> Result<WhereClause, NeverClauseError> {
    create_clause_with_default_and_ignored(target, generic_params, default_bounds, None)
}
fn create_clause_with_default_and_ignored(
    target: &Option<TraitRequirements>,
    generic_params: &[GenericParam],
    default_bounds: Vec<TypeParamBound>,
    mut should_ignore: Option<&mut dyn FnMut(&GenericParam) -> bool>,
) -> Result<WhereClause, NeverClauseError> {
    Ok(match *target {
        Some(TraitRequirements::Never { span }) => {
            // do not implement
            return Err(NeverClauseError { span });
        }
        Some(TraitRequirements::Where(ref explicit)) => explicit.clone(),
        Some(TraitRequirements::Always { span: _ }) => {
            // Absolutely no conditions on implementation
            empty_clause()
        }
        None => {
            let mut where_clause = empty_clause();
            // Infer bounds for all params
            for param in generic_params {
                if let Some(ref mut should_ignore) = should_ignore {
                    if should_ignore(param) {
                        continue;
                    }
                }
                if let GenericParam::Type(ref t) = param {
                    let ident = &t.ident;
                    where_clause
                        .predicates
                        .push(WherePredicate::Type(syn::PredicateType {
                            bounded_ty: parse_quote!(#ident),
                            colon_token: Default::default(),
                            bounds: default_bounds.iter().cloned().collect(),
                            lifetimes: None,
                        }))
                }
            }
            where_clause
        }
    })
}

/// Indicates that [`TraitRequirements::Never`] was encountered,
/// explicitly disabling the clause
#[derive(Debug)]
struct NeverClauseError {
    span: Span,
}
