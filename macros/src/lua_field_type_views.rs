//! `#[derive(LuaFieldTypeViews)]` — emit a base class + per-discriminator
//! subclass family for a "catch-all" struct whose fields apply to
//! different subsets of variants (typically `FieldDefinition`).
//!
//! The derive emits a single `render_lua_field_type_views(out)` function
//! that writes:
//!
//! 1. The base `--- @class crap.BaseField` block with every field that
//!    has no `#[lua(applies_to = "...")]` annotation.
//! 2. One `--- @class crap.XField : crap.BaseField` block per
//!    discriminator variant carrying
//!    `#[lua(view_class = "crap.XField")]`, with only the fields whose
//!    `applies_to` list includes that variant's slug.

use darling::{FromDeriveInput, ast};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, DeriveInput, parse_macro_input};

use crate::shared::{
    LuaField, build_class_header, build_field_emit, extract_docs, from_derive_input_or_return,
    strip_option,
};

#[derive(FromDeriveInput)]
#[darling(attributes(lua), supports(struct_named), forward_attrs(doc))]
struct LuaFieldTypeViewsContainer {
    ident: syn::Ident,
    data: ast::Data<(), LuaField>,
    attrs: Vec<Attribute>,
    /// Base class name (e.g. `"crap.BaseField"`) — emitted first, holds
    /// all fields with no `applies_to`. Per-view subclasses extend this.
    base: String,
    /// Path to the discriminator enum (e.g. `FieldType`). The macro
    /// references this enum via the `LuaFieldTypeViewsDiscriminator`
    /// trait at runtime — the enum itself supplies the (slug → view-class)
    /// mapping via that trait's `VIEWS` table.
    discriminator: syn::Path,
    /// Accepted (but unused here) so the same struct can also derive
    /// `LuaAnnotation` and share the `#[lua(...)]` attribute container.
    #[darling(default)]
    #[allow(dead_code)]
    class: Option<String>,
    #[darling(default)]
    #[allow(dead_code)]
    extends: Option<String>,
    #[darling(default)]
    #[allow(dead_code)]
    rename_all: Option<String>,
    #[darling(default)]
    #[allow(dead_code)]
    extra_field: Option<String>,
}

pub(crate) fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let container = from_derive_input_or_return!(LuaFieldTypeViewsContainer, &input);

    let ident = &container.ident;
    let base_class = &container.base;
    let discriminator = &container.discriminator;
    let struct_docs = extract_docs(&container.attrs);

    let Some(struct_fields) = container.data.take_struct() else {
        unreachable!("supports(struct_named) guarantees this");
    };
    let fields = struct_fields.fields;

    // Honor `rename_all` the same way `LuaAnnotation` does — both derive off the
    // same shared `#[lua(...)]` container, so the emitted field names must agree.
    let rename_all = container.rename_all.as_deref();

    // ── BaseField: every field with no `applies_to` (and not `skip`)
    let base_header = build_class_header(base_class, None, &struct_docs);
    let mut base_stmts: Vec<TokenStream2> = Vec::new();
    for f in &fields {
        if f.skip || f.applies_to.is_some() {
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
            Ok(s) => base_stmts.extend(s),
            Err(e) => return e.write_errors().into(),
        }
    }

    // ── Per-view: bucket fields by `applies_to` slug, emit one match
    //    arm per unique slug. At runtime, iterate the discriminator's
    //    VIEWS table and dispatch on each slug.
    let mut slug_to_fields: std::collections::BTreeMap<String, Vec<&LuaField>> =
        std::collections::BTreeMap::new();
    for f in &fields {
        if f.skip {
            continue;
        }
        let Some(applies) = &f.applies_to else {
            continue;
        };
        for slug in applies.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            slug_to_fields.entry(slug.to_string()).or_default().push(f);
        }
    }

    let mut match_arms: Vec<TokenStream2> = Vec::new();
    for (slug, fs) in &slug_to_fields {
        let mut arm_stmts: Vec<TokenStream2> = Vec::new();
        for f in fs {
            let Some(name) = f.ident.as_ref() else {
                continue;
            };
            if f.flatten {
                // `#[lua(flatten)]`: instead of emitting one
                // `--- @field` line, call the inner type's
                // `LuaFieldBlock` to inline its field list directly
                // into the subclass body. The inner type is
                // `Option<T>` unwrapped (so `flatten` works on
                // optional nested structs too).
                let (_, inner_ty) = strip_option(&f.ty);
                arm_stmts.push(quote! {
                    <#inner_ty as crate::typegen::lua::LuaFieldBlock>::render_lua_fields_only(out);
                });
                continue;
            }
            let lua_name = f
                .rename
                .clone()
                .unwrap_or_else(|| crate::shared::apply_rename_all(&name.to_string(), rename_all));
            match build_field_emit(&lua_name, f) {
                Ok(s) => arm_stmts.extend(s),
                Err(e) => return e.write_errors().into(),
            }
        }
        match_arms.push(quote! {
            #slug => { #(#arm_stmts)* }
        });
    }

    let expanded = quote! {
        impl LuaFieldTypeViews for #ident {
            fn render_lua_field_type_views(out: &mut ::std::string::String) {
                // 1. Base class block — all common fields.
                out.push_str(#base_header);
                #(#base_stmts)*
                out.push('\n');

                // 2. Per-variant subclasses — driven by the discriminator's
                //    `VIEWS` table at runtime. Fully-qualified path so
                //    users don't need to import
                //    `LuaFieldTypeViewsDiscriminator`.
                for (slug, class)
                    in <#discriminator as crate::typegen::lua::LuaFieldTypeViewsDiscriminator>::VIEWS
                {
                    out.push_str("--- @class ");
                    out.push_str(class);
                    out.push_str(" : ");
                    out.push_str(#base_class);
                    out.push('\n');
                    match *slug {
                        #(#match_arms)*
                        _ => {}
                    }
                    out.push('\n');
                }
            }
        }
    };

    expanded.into()
}
