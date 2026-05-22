//! `lua_table! { name, path, state, fns, header? }` — function-like macro
//! that declares a Lua API table.
//!
//! Generates two `pub(crate) fn`s:
//! - `register_{name}(lua: &Lua, state: {state}) -> mlua::Result<()>` —
//!   creates the table (via `ensure_table`) and wires each `fns` entry
//!   into it under its `path`'s last segment.
//! - `render_{name}_lua(out: &mut String)` — appends each fn's
//!   `function ... end` block via `format_lua_fn_spec`. When the
//!   optional `header:` attr is set, the namespace preamble
//!   (`-- ── crap.X ──` divider + intro doc + `--- @class crap.X` +
//!   `crap.X = {}`) is emitted first.
//!
//! Both `register_*` and `render_*` are idempotent w.r.t. table
//! creation — `ensure_table` reuses existing parent tables when sibling
//! `lua_table!` calls have already created them.
//!
//! ## Linkage to `#[lua_fn]`
//!
//! Each ident in `fns: [...]` must name a fn carrying `#[lua_fn(...)]`
//! in the calling module's scope — the macro expands to references to
//! the `<NAME>_SPEC` const and `<name>_register` wrapper that
//! `#[lua_fn]` emits. Proc macros run before name resolution, so a
//! typo can't be diagnosed here; the resulting rustc error reads
//! `cannot find value [TYPO]_SPEC in this scope` and points at the
//! `lua_table!` call site. If you see that, check the ident in `fns:`
//! against the matching `#[lua_fn] fn ...` declaration.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Type, parse_macro_input};

pub(crate) fn run(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as LuaTableInput);

    let register_name = format_ident!("register_{}", parsed.name);
    let render_name = format_ident!("render_{}_lua", parsed.name);

    let spec_names: Vec<_> = parsed
        .fns
        .iter()
        .map(|f| format_ident!("{}_SPEC", f.to_string().to_uppercase()))
        .collect();
    let register_fn_names: Vec<_> = parsed
        .fns
        .iter()
        .map(|f| format_ident!("{}_register", f))
        .collect();

    let path_str = parsed.path.value();
    let segs: Vec<_> = path_str.split('.').map(String::from).collect();
    let state_ty = &parsed.state;

    let header_emit = match &parsed.header {
        Some(doc) => {
            let doc_str = doc.value();
            quote! {
                ::crap_cms::typegen::lua::format_lua_section_header(out, #path_str, #doc_str);
            }
        }
        None => quote! {},
    };

    let expanded = quote! {
        pub(crate) fn #register_name(
            lua: &::mlua::Lua,
            state: #state_ty,
        ) -> ::mlua::Result<()> {
            let state = ::std::sync::Arc::new(state);
            let segs: &[&str] = &[#(#segs),*];
            let t = ::crap_cms::typegen::lua::ensure_table(lua, segs)?;
            #(
                let fn_obj = #register_fn_names(lua, ::std::sync::Arc::clone(&state))?;
                t.set(#spec_names.last_segment(), fn_obj)?;
            )*
            Ok(())
        }

        #[allow(dead_code)]
        pub(crate) fn #render_name(out: &mut ::std::string::String) {
            #header_emit
            #(
                ::crap_cms::typegen::lua::format_lua_fn_spec(out, &#spec_names);
            )*
        }
    };

    expanded.into()
}

struct LuaTableInput {
    name: syn::Ident,
    path: syn::LitStr,
    state: Type,
    fns: Vec<syn::Ident>,
    /// Optional section-header doc. When set, the render fn emits the
    /// `-- ── crap.X ─...─` divider + `--- <doc>` lines + `--- @class
    /// crap.X` + `crap.X = {}` initializer before the fn blocks — so
    /// the namespace stub doesn't have to live in a separate `.lua`
    /// block.
    header: Option<syn::LitStr>,
}

impl syn::parse::Parse for LuaTableInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut path = None;
        let mut state = None;
        let mut fns = None;
        let mut header = None;

        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![:]>()?;

            match key.to_string().as_str() {
                "name" => name = Some(input.parse()?),
                "path" => path = Some(input.parse()?),
                "state" => state = Some(input.parse()?),
                "header" => header = Some(input.parse()?),
                "fns" => {
                    let content;
                    syn::bracketed!(content in input);
                    let punct: syn::punctuated::Punctuated<syn::Ident, syn::Token![,]> =
                        content.parse_terminated(syn::Ident::parse, syn::Token![,])?;
                    fns = Some(punct.into_iter().collect());
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "lua_table!: unknown key `{other}` (expected one of: name, path, state, fns, header)"
                        ),
                    ));
                }
            }

            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }

        Ok(LuaTableInput {
            name: name.ok_or_else(|| {
                syn::Error::new(proc_macro2::Span::call_site(), "lua_table!: missing `name`")
            })?,
            path: path.ok_or_else(|| {
                syn::Error::new(proc_macro2::Span::call_site(), "lua_table!: missing `path`")
            })?,
            state: state.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "lua_table!: missing `state`",
                )
            })?,
            fns: fns.unwrap_or_default(),
            header,
        })
    }
}
