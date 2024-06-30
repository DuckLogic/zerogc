mod collect_impl;
pub(crate) mod helpers;

#[proc_macro]
pub fn unsafe_collect_impl(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let parsed = syn::parse_macro_input!(input as collect_impl::MacroInput);
    let res = parsed
        .expand_output()
        .unwrap_or_else(|e| e.to_compile_error());
    res.into()
}
