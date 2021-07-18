use proc_macro2::{Ident, TokenStream};
use syn::{Type, Expr, ItemImpl, Path, ImplItem, Lifetime};

use darling::{FromMeta, FromField};
use darling::util::SpannedValue;
use quote::ToTokens;

#[derive(FromMeta, Debug)]
struct MutableFieldOpts {
    public: darling::util::Flag,
}
#[derive(FromField, Debug)]
struct TraceField {
    ident: Option<Ident>,
    ty: Type,
    /// Mark the field as mutable,
    /// needing a generated write barrier.
    mutable: Option<SpannedValue<MutableFieldOpts>>,
    // Flag to mark a field as (unsafely) skipped
    unsafe_skip_trace: darling::util::Flag,
}
#[derive(FromTypeParam, Debug)]
#[darling(attributes(zerogc))]
struct TracedTypeParam {
    /// The identifier of the passed in type parameter
    ident: Ident,
    /// The declared bounds of the type parameter.
    bounds: Vec<syn::Typ>,
    /// Whether to ignore this type parameter in generated trace bounds
    ignore: darling::util::Flag
}

#[derive(FromVariant, Debug)]
struct TraceVariant {
    ident: Ident,
    fields: darling::ast::Fields<TraceField>
}
#[derive(FromMeta)]
struct LifetimeAttrs {
    /// Mark this lifetime as ignored
    ignore: darling::util::Flag,
    /// Mark this lifetime as used for gc
    #[darling(rename = "ctx")]
    used_for_context: darling::util::Flag
}

#[derive(FromDeriveInput, Debug)]
struct DeriveTrace {
    ident: Ident,
    generics: darling::ast::Generics<darling::ast::GenericParam<TracedTypeParam>>,
    data: darling::ast::Data<TraceVariant, TraceField>,
    #[darling(rename = "copy")]
    is_copy: darling::util::Flag,
    nop_trace: darling::util::Flag,
    #[darling(rename = "gc_lifetime")]
    gc_lifetime_name: Option<Ident>
    collector_id: Ident,
    /// Unsafely assume the type is safe to [drop]
    /// from a GC, as consistent with the requirements of [GcSafe]
    ///
    /// This 'skips' the generation of a dummy drop
    unsafe_skip_drop: darling::util::Flag
}

impl DeriveTrace {
    fn generate_trace_impl(&self) -> TraceImpl {
        le
    }
}
pub struct AbstractImpl {
    /// The name of the crate, where everything is imported from
    zerogc_crate: Ident,
    /// The identifier of the target struct
    target_ident: Ident,
    /// The generics, implicitly including the bounds for the implementation
    impl_generics: syn::Generics
}
impl AbstractImpl {
    fn generate_impl(&self, target_trait: Path, inner_items: Vec<ImplItem>) -> ItemImpl {
        let crate_name = &self.zerogc_crate;
        let (impl_generics, ty_generics, where_clause) = self.impl_generics.split_for_impl();
        let inside = func(self);
        quote! {
            impl #impl_generics #target_trait for #target_ident #ty_generics #where_clause {
                #inside
            }
        }
    }
}
pub struct TraceImpl {
    base: AbstractImpl,
    /// The value of `const NEEDS_TRACE: bool`
    needs_trace: Expr,
    /// The value of `const NEEDS_DROP: bool`
    needs_drop: Expr,
    /// The implementation of `Trace::visit`
    visit_method: Expr,
}
impl TraceImpl {
    fn generate_impl(&self) -> ItemImpl {
        let needs_trace = &self.needs_trace;
        let needs_drop = &self.needs_drop;
        let visit_method = &self.visit_method;
        let crate_name = &self.base.zerogc_crate;
        self.base.generate_impl(
            parse_quote!(#crate_name::Trace),
            vec![
                parse_quote!(const NEEDS_TRACE: bool = #needs_trace;),
                parse_quote!(const NEEDS_DROP: bool = #needs_drop;),
                parse_quote!(fn visit<Visitor: #crate_name::GcVisitor>(&mut self, visitor: &mut Visitor) -> Result<(), Visitor::Err> {
                    #visit_method
                })
            ]
        )
    }
}
#[derive(Debug, Copy, Clone)]
enum BrandKind {
    Erase,
    Rebrand
}
impl BrandKind {
    fn full_path(&self, crate_name: &Ident, collector_id: &Type) -> Path {
        let simple_name = self.simple_name();
        let lt = self.new_lifetime();
        parse_quote!(#crate_name::#simple_name::<#lt, #collector_id>)
    }
    fn simple_name(&self) -> Ident {
        match *self {
            BrandKind::Erase => parse_quote!(GcErase),
            BrandKind::Rebrand => parse_quote!(GcRebrand)
        }
    }
    fn new_lifetime(&self) -> Lifetime {
        match *self {
            BrandKind::Erase => parse_quote!('min),
            BrandKind::Rebrand => parse_quote!('new_gc)
        }
    }
    fn assoc_type(&self) -> Ident {
        match *self {
            BrandKind::Erase => parse_quote!(Erased),
            BrandKind::Rebrand => parse_quote!(Branded)
        }
    }
}
pub struct BrandingImpl {
    base: AbstractImpl,
    kind: BrandKind,
    collector_id: Type,
    branded_type_res: Type
}
impl BrandingImpl {
    fn generate_impl(&self) -> ItemImpl {
        let assoc_type = self.kind.assoc_type();
        let branded_type_res = &self.branded_type_res;
        self.base.generate_impl(
            self.kind.full_path(&self.base.zerogc_crate, &self.collector_id),
            vec![
                parse_quote!(type #assoc_type = #branded_type_res;)
            ]
        )
    }
}
struct GcSafeImpl {
    base: AbstractImpl
}
impl GcSafeImpl {
    fn generate_impl(&self) -> ItemImpl {
        let zerogc_crate = &self.base.zerogc_crate;
        self.base.generate_impl(
            parse_quote!(#zerogc_crate::GcSafe),
            vec![]
        )
    }
}
