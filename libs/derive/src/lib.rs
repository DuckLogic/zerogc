#![feature(
    proc_macro_tracked_env, // Used for `DEBUG_DERIVE`
    proc_macro_span, // Used for source file ids
    proc_macro_diagnostic, // Used for warnings
)]
extern crate proc_macro;

use quote::{quote, ToTokens};
use syn::parse::Parse;
use syn::punctuated::Punctuated;
use syn::{DeriveInput, GenericParam, Generics, Token, Type, WhereClause, parse_macro_input, parse_quote};
use proc_macro2::{TokenStream, Span};
use darling::{FromDeriveInput, FromMeta};
use std::fmt::Display;
use std::io::Write;
use crate::derive::TraceDeriveKind;

mod macros;
mod derive;

/// Magic const that expands to either `::zerogc` or `crate::`
/// depending on whether we are currently bootstrapping (compiling `zerogc` itself)
///
/// This is equivalent to `$crate` for regular macros
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

/// Sort the parameters so that lifetime parameters come before
/// type parameters, and type parameters come before const paramaters
pub(crate) fn sort_params(generics: &mut Generics) {
    #[derive(Eq, PartialEq, Ord, PartialOrd, Copy, Clone, Debug)]
    enum ParamOrder {
        Lifetime,
        Type,
        Const,
    }
    let mut pairs = std::mem::take(&mut generics.params).into_pairs()
        .collect::<Vec<_>>();
    use syn::punctuated::Pair;
    pairs.sort_by_key(|pair| {
        match pair.value() {
            syn::GenericParam::Lifetime(_) => ParamOrder::Lifetime,
            syn::GenericParam::Type(_) => ParamOrder::Type,
            syn::GenericParam::Const(_) => ParamOrder::Const,
        }
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
            pairs.insert(old_ending_index, Pair::Punctuated(value, Default::default()));
        }
    }
    generics.params = pairs.into_iter().collect();
}

pub(crate) fn emit_warning(msg: impl ToString, span: Span) {
    let mut d = proc_macro::Diagnostic::new(proc_macro::Level::Warning, msg.to_string());
    d.set_spans(span.unwrap());
    d.emit();

}

pub(crate) fn move_bounds_to_where_clause(mut generics: Generics) -> Generics {
    let where_clause = generics.where_clause.get_or_insert_with(|| WhereClause {
        where_token: Default::default(),
        predicates: Default::default()
    });
    for param in &mut generics.params {
        match *param {
            GenericParam::Lifetime(ref mut lt) => {
                let target = &lt.lifetime;
                let bounds = &lt.bounds;
                if !bounds.is_empty() {
                    where_clause.predicates.push(parse_quote!(#target: #bounds));
                }
                lt.colon_token = None;
                lt.bounds.clear();
            },
            GenericParam::Type(ref mut tp) => {
                let bounds = &tp.bounds;
                let target = &tp.ident;
                if !bounds.is_empty() {
                    where_clause.predicates.push(parse_quote!(#target: #bounds));
                }
                tp.eq_token = None;
                tp.colon_token = None;
                tp.default = None;
                tp.bounds.clear();
            },
            GenericParam::Const(ref mut c) => {
                c.eq_token = None;
                c.default = None;
            }
        }
    }
    if generics.where_clause.as_ref().map_or(false, |clause| clause.predicates.is_empty()) {
        generics.where_clause = None;
    }
    generics
}

#[proc_macro]
pub fn unsafe_gc_impl(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let parsed = parse_macro_input!(input as macros::MacroInput);
    let res = parsed.expand_output()
        .unwrap_or_else(|e| e.to_compile_error());
    let tp = match parsed.target_type {
        Type::Path(ref p) => Some(&p.path.segments.last().unwrap().ident),
        _ => None
    };
    let span_loc = span_file_loc(Span::call_site());
    let f = if let Some(tp) = tp {
        format!("unsafe_gc_impl!(target={}, ...) @ {}", tp, span_loc)
    } else {
        format!("unsafe_gc_impl! @ {}", span_loc)
    };
    debug_derive(
        "unsafe_gc_impl!",
        &tp.map_or_else(|| span_loc.to_string(), |tp| tp.to_string()),
        &f,
        &res
    );
    res.into()
}

#[proc_macro_derive(NullTrace, attributes(zerogc))]
pub fn derive_null_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let res = From::from(impl_derive_trace(&input,  TraceDeriveKind::NullTrace)
        .unwrap_or_else(|e| e.write_errors()));
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
    let res = From::from(impl_derive_trace(&input, TraceDeriveKind::Regular)
        .unwrap_or_else(|e| e.write_errors()));
    debug_derive(
        "derive(Trace)",
        &input.ident.to_string(),
        &format_args!("#[derive(Trace) for {}", input.ident),
        &res
    );
    res
}

pub(crate) const DESERIALIZE_ENABLED: bool = cfg!(feature = "__serde-internal");

#[proc_macro_derive(GcDeserialize, attributes(zerogc))]
pub fn gc_deserialize(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let res = From::from(impl_derive_trace(&input, TraceDeriveKind::Deserialize)
        .unwrap_or_else(|e| e.write_errors()));
    debug_derive(
        "derive(GcDeserialize)",
        &input.ident.to_string(),
        &format_args!("#[derive(GcDeserialize) for {}", input.ident),
        &res
    );
    res
}

fn impl_derive_trace(input: &DeriveInput, kind: TraceDeriveKind) -> Result<TokenStream, darling::Error> {
    let mut input = derive::TraceDeriveInput::from_derive_input(input)?;
    input.normalize(kind)?;
    input.expand(kind)
}

/// A list like `#[zerogc(a, b, c)] parsed as a `Punctuated<T, Token![,]>`,
#[derive(Clone, Debug)]
pub(crate) struct MetaList<T>(pub Punctuated<T, syn::token::Comma>);
impl<T> Default for MetaList<T> {
    fn default() -> Self {
        MetaList(Default::default())
    }
}
impl<T: Parse> FromMeta for MetaList<T> {
    fn from_list(items: &[syn::NestedMeta]) -> darling::Result<Self> {
        let mut res: Punctuated<T, Token![,]> = Default::default();
        for item in items {
            res.push(syn::parse2(item.to_token_stream())?);

        }
        Ok(MetaList(res))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FromLitStr<T>(pub T);
impl<T: Parse> Parse for FromLitStr<T> {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let s = input.parse::<syn::LitStr>()?;
        Ok(FromLitStr(s.parse()?))
    }
}

pub(crate) fn is_explicitly_unsized(param: &syn::TypeParam) -> bool {
    param.bounds.iter().any(|bound| {
        matches!(bound, syn::TypeParamBound::Trait(syn::TraitBound {
            ref path, modifier: syn::TraitBoundModifier::Maybe(_), ..
        }) if path.is_ident("Sized"))
    })
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
    let lineno = internal.start().line();
    format!("{}:{}", file_name, lineno)
}

fn debug_derive(key: &str, target: &dyn ToString, message: &dyn Display, value: &dyn Display) {
    let target = target.to_string();
    // TODO: Use proc_macro::tracked_env::var
    match ::proc_macro::tracked_env::var("DEBUG_DERIVE") {
        Ok(ref var) if var == "*" || var == "1" || var.is_empty() => {}
        Ok(ref var) if var == "0" => { return /* disabled */ }
        Ok(var) => {
            let target_parts = std::iter::once(key)
                .chain(target.split(':')).collect::<Vec<_>>();
            for pattern in var.split_terminator(',') {
                let pattern_parts = pattern.split(':').collect::<Vec<_>>();
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
