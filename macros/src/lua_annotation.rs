//! `#[derive(LuaAnnotation)]` — emit `--- @class crap.X` from a
//! named-field struct.
//!
//! See [`crate`] for the full attribute reference. The derive also emits
//! a companion `impl LuaFieldBlock` so every annotated type can be a
//! flatten target for [`LuaFieldTypeViews`].

use darling::{FromDeriveInput, ast};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, DeriveInput, Generics, parse_macro_input};

use crate::shared::{
    LuaField, build_class_header, build_field_emit, extract_docs, from_derive_input_or_return,
};

#[derive(FromDeriveInput)]
#[darling(attributes(lua), supports(struct_named), forward_attrs(doc))]
struct LuaContainer {
    ident: syn::Ident,
    generics: Generics,
    data: ast::Data<(), LuaField>,
    attrs: Vec<Attribute>,
    class: String,
    #[darling(default)]
    extends: Option<String>,
    /// Optional `rename_all` strategy applied to field names that lack
    /// an explicit `#[lua(rename = "...")]`. Mirrors serde's
    /// `rename_all` (subset): `"snake_case"`, `"lowercase"`,
    /// `"camelCase"`, `"kebab-case"`. Per-field `#[lua(rename)]`
    /// always wins.
    #[darling(default)]
    rename_all: Option<String>,
    /// Optional `extra_field = "[K] V"` literal(s) appended to the class as
    /// final `--- @field [K] V` line(s). Used for `lua-language-server` index
    /// signatures (e.g. `crap.Document`'s `[string] any`) and for context-table
    /// keys injected at marshal time rather than present on the Rust struct
    /// (e.g. `hook_depth`, `options` on `crap.HookContext`). May be repeated;
    /// each is emitted verbatim, in order, after the regular fields.
    #[darling(multiple)]
    extra_field: Vec<String>,
    /// Accepted (but unused here) so a struct may stack `LuaAnnotation`
    /// alongside `LuaFieldTypeViews` and share the `#[lua(...)]`
    /// attribute container.
    #[darling(default)]
    #[allow(dead_code)]
    base: Option<String>,
    #[darling(default)]
    #[allow(dead_code)]
    discriminator: Option<syn::Path>,
}

pub(crate) fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let container = from_derive_input_or_return!(LuaContainer, &input);

    let ident = &container.ident;
    let (impl_generics, ty_generics, where_clause) = container.generics.split_for_impl();
    let class = &container.class;
    let extends = container.extends.as_deref();
    let rename_all = container.rename_all.as_deref();
    let extra_fields = &container.extra_field;
    let struct_docs = extract_docs(&container.attrs);

    let Some(struct_fields) = container.data.take_struct() else {
        unreachable!("supports(struct_named) guarantees this");
    };
    let fields = struct_fields.fields;

    // ── Header: struct doc-comments + `--- @class` line
    let header = build_class_header(class, extends, &struct_docs);

    // ── Body: one set of statements per field
    let mut stmts: Vec<TokenStream2> = Vec::new();
    for f in &fields {
        if f.skip {
            continue;
        }
        let Some(name) = f.ident.as_ref() else {
            continue;
        };
        let lua_name = f
            .rename
            .clone()
            .unwrap_or_else(|| crate::shared::apply_rename_all(&name.to_string(), rename_all));
        match build_field_emit(&lua_name, f) {
            Ok(s) => stmts.extend(s),
            Err(e) => return e.write_errors().into(),
        }
    }

    let extra_field_emit = {
        let lines: Vec<_> = extra_fields
            .iter()
            .map(|decl| {
                let line = format!("--- @field {decl}\n");
                quote! { out.push_str(#line); }
            })
            .collect();
        quote! { #(#lines)* }
    };

    let expanded = quote! {
        impl #impl_generics LuaAnnotation for #ident #ty_generics #where_clause {
            const CLASS_NAME: &'static str = #class;

            fn render_lua_annotation(out: &mut ::std::string::String) {
                out.push_str(#header);
                <Self as crate::typegen::lua::LuaFieldBlock>::render_lua_fields_only(out);
                #extra_field_emit
                out.push('\n');
            }
        }

        // Free companion impl: same field-emit logic, no class header.
        // Consumed by `LuaFieldTypeViews`'s `#[lua(flatten)]` to inline
        // a type's fields into another class's body. Always emitted so
        // any `LuaAnnotation` type can be a flatten target.
        impl #impl_generics crate::typegen::lua::LuaFieldBlock for #ident #ty_generics #where_clause {
            fn render_lua_fields_only(out: &mut ::std::string::String) {
                #(#stmts)*
            }
        }
    };

    expanded.into()
}
