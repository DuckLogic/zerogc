//! Helpers for macros, potentially shared across implementations.
use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Error, GenericArgument, Generics, Type};

pub fn combine_errors(errors: Vec<syn::Error>) -> Result<(), syn::Error> {
    let mut iter = errors.into_iter();
    if let Some(mut first) = iter.next() {
        for other in iter {
            first.combine(other);
        }
        Err(first)
    } else {
        Ok(())
    }
}

pub fn rewrite_type(
    target: &Type,
    target_type_name: &str,
    rewriter: &mut dyn FnMut(&Type) -> Option<Type>,
) -> Result<Type, Error> {
    if let Some(explicitly_rewritten) = rewriter(target) {
        return Ok(explicitly_rewritten);
    }
    let mut target = target.clone();
    match target {
        Type::Paren(ref mut inner) => {
            *inner.elem = rewrite_type(&inner.elem, target_type_name, rewriter)?
        }
        Type::Group(ref mut inner) => {
            *inner.elem = rewrite_type(&inner.elem, target_type_name, rewriter)?
        }
        Type::Reference(ref mut target) => {
            // TODO: Lifetime safety?
            // Rewrite reference target
            *target.elem = rewrite_type(&target.elem, target_type_name, rewriter)?
        }
        Type::Path(syn::TypePath {
            ref mut qself,
            ref mut path,
        }) => {
            *qself = qself
                .clone()
                .map::<Result<_, Error>, _>(|mut qself| {
                    qself.ty = Box::new(rewrite_type(&qself.ty, target_type_name, &mut *rewriter)?);
                    Ok(qself)
                })
                .transpose()?;
            path.segments = path
                .segments
                .iter()
                .cloned()
                .map(|mut segment| {
                    // old_segment.ident is ignored...
                    match segment.arguments {
                        syn::PathArguments::None => {} // Nothing to do here
                        syn::PathArguments::AngleBracketed(ref mut generic_args) => {
                            for arg in &mut generic_args.args {
                                match arg {
                                    GenericArgument::Lifetime(_) | GenericArgument::Const(_) => {}
                                    GenericArgument::Type(ref mut generic_type) => {
                                        *generic_type = rewrite_type(
                                            generic_type,
                                            target_type_name,
                                            &mut *rewriter,
                                        )?;
                                    }
                                    // TODO: Handle other generic ?
                                    _ => {
                                        return Err(Error::new(
                                            arg.span(),
                                            format!(
                                            "Unable to handle generic arg while rewriting as a {}",
                                            target_type_name
                                        ),
                                        ))
                                    }
                                }
                            }
                        }
                        syn::PathArguments::Parenthesized(ref mut paran_args) => {
                            return Err(Error::new(
                                paran_args.span(),
                                "TODO: Rewriting parenthesized (fn-style) args",
                            ));
                        }
                    }
                    Ok(segment)
                })
                .collect::<Result<_, Error>>()?;
        }
        _ => {
            return Err(Error::new(
                target.span(),
                format!(
                    "Unable to rewrite type as a `{}`: {}",
                    target_type_name,
                    quote!(#target)
                ),
            ))
        }
    }
    Ok(target)
}

/// This refers to the zerogc_next crate,
/// equivalent to `$crate` for proc_macros
pub fn zerogc_next_crate() -> TokenStream {
    /*
     * TODO: There is no way to emulate for `$crate` for proc_macros
     *
     * Instead we re-export `extern crate self as zerogc_next` at the root of the zerogc_next crate.
     */
    quote!(zerogc_next::)
}

// Sort the parameters so that lifetime parameters come before
/// type parameters, and type parameters come before const paramaters
pub fn sort_params(generics: &mut Generics) {
    #[derive(Eq, PartialEq, Ord, PartialOrd, Copy, Clone, Debug)]
    enum ParamOrder {
        Lifetime,
        Type,
        Const,
    }
    let mut pairs = std::mem::take(&mut generics.params)
        .into_pairs()
        .collect::<Vec<_>>();
    use syn::punctuated::Pair;
    pairs.sort_by_key(|pair| match pair.value() {
        syn::GenericParam::Lifetime(_) => ParamOrder::Lifetime,
        syn::GenericParam::Type(_) => ParamOrder::Type,
        syn::GenericParam::Const(_) => ParamOrder::Const,
    });
    /*
     * NOTE: The `Pair::End` can only come at the end.
     * Now that we've sorted, it's possible the old ending
     * could be in the first position or some other position
     * before the end.
     *
     * If that's the case, then add punctuation to the end.
     */
    if let Some(old_ending_index) = pairs.iter().position(|p| p.punct().is_none()) {
        if old_ending_index != pairs.len() - 1 {
            let value = pairs.remove(old_ending_index).into_value();
            pairs.insert(
                old_ending_index,
                Pair::Punctuated(value, Default::default()),
            );
        }
    }
    generics.params = pairs.into_iter().collect();
}
