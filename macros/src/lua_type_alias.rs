//! `#[derive(LuaTypeAlias)]` — emit `--- @alias crap.X <target>` from a
//! unit struct. Use for aliases whose target shape doesn't fit the
//! enum-driven `LuaAlias` modes — most commonly `fun(...)` callable
//! aliases.
//!
//! ```ignore
//! /// Custom validation function type.
//! /// Return nil or true if valid, return a string error message if invalid.
//! #[derive(LuaTypeAlias)]
//! #[lua(
//!     alias = "crap.ValidateFunction",
//!     target = "fun(value: any, ctx: crap.ValidateContext): string?",
//! )]
//! pub struct ValidateFunction;
//! ```

use darling::FromDeriveInput;
use proc_macro::TokenStream;
use quote::quote;
use syn::{Attribute, DeriveInput, parse_macro_input};

use crate::shared::extract_docs;

#[derive(FromDeriveInput)]
#[darling(attributes(lua), supports(struct_unit), forward_attrs(doc))]
struct LuaTypeAliasContainer {
    ident: syn::Ident,
    attrs: Vec<Attribute>,
    alias: String,
    target: String,
}

pub(crate) fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let container = match LuaTypeAliasContainer::from_derive_input(&input) {
        Ok(c) => c,
        Err(e) => return e.write_errors().into(),
    };

    let ident = &container.ident;
    let alias = &container.alias;
    let target = &container.target;
    let docs = extract_docs(&container.attrs);

    let mut body = String::new();
    for line in &docs {
        if line.is_empty() {
            body.push_str("---\n");
        } else {
            body.push_str("--- ");
            body.push_str(line);
            body.push('\n');
        }
    }
    body.push_str("--- @alias ");
    body.push_str(alias);
    body.push(' ');
    body.push_str(target);
    body.push_str("\n\n");

    let expanded = quote! {
        impl LuaAlias for #ident {
            const ALIAS_NAME: &'static str = #alias;

            fn render_lua_alias(out: &mut ::std::string::String) {
                out.push_str(#body);
            }
        }
    };

    expanded.into()
}
