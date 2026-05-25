//! `#[derive(LuaTaggedClass)]` — emit one `--- @class crap.X<Variant>`
//! per variant of a Rust tagged-enum + a `--- @alias crap.X` union.
//!
//! Each variant becomes its own Lua class with the variant's fields as
//! `--- @field` lines. When the container declares a `tag = "..."`
//! attribute, every variant class also carries a `--- @field <tag>
//! "<variant-lua-name>"` literal so `LuaLS` can narrow on the
//! discriminator. Variant names go through `rename_all` /
//! `#[lua(rename = "...")]` the same way `LuaAlias` handles them.
//!
//! ```rust,ignore
//! #[derive(LuaTaggedClass)]
//! #[lua(class = "crap.AuthMethod", tag = "type", rename_all = "snake_case")]
//! pub enum AuthMethod {
//!     PasswordLogin { mfa: MfaMode, verify_email: bool },
//!     Bearer { surfaces: SurfaceSet },
//! }
//! ```
//!
//! Emits:
//!
//! ```text
//! --- @class crap.AuthMethodPasswordLogin
//! --- @field type "password_login"
//! --- @field mfa? crap.MfaMode
//! --- @field verify_email? boolean
//!
//! --- @class crap.AuthMethodBearer
//! --- @field type "bearer"
//! --- @field surfaces? crap.Surface[]
//!
//! --- @alias crap.AuthMethod
//! --- | crap.AuthMethodPasswordLogin
//! --- | crap.AuthMethodBearer
//! ```
//!
//! `tag` omitted = untagged enum (each variant class lists only its own
//! fields, no discriminator).

use darling::{FromDeriveInput, FromVariant, ast};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, DeriveInput, parse_macro_input};

use crate::shared::{
    LuaField, apply_rename_all, build_class_header, build_field_emit, extract_docs, push_doc_line,
};

#[derive(FromDeriveInput)]
#[darling(attributes(lua), supports(enum_any), forward_attrs(doc, serde))]
struct LuaTaggedClassContainer {
    ident: syn::Ident,
    data: ast::Data<LuaTaggedClassVariant, ()>,
    attrs: Vec<Attribute>,
    /// Lua union alias name (e.g. `"crap.AuthMethod"`). Each per-variant
    /// class is named `<class><VariantIdent>` and the alias lists them
    /// in declaration order.
    class: String,
    /// Discriminator field name (e.g. `"type"`). When set, each variant
    /// class gets a `--- @field <tag> "<variant-lua-name>"` line as its
    /// first field. Omit for `serde(untagged)`-style enums.
    ///
    /// Defaults to whatever `#[serde(tag = "...")]` declares on the
    /// same enum — override here only when the Lua-side discriminator
    /// needs to differ from the serde-side one (rare).
    #[darling(default)]
    tag: Option<String>,
    /// Variant-name rename strategy (same vocabulary as
    /// `LuaAlias::rename_all` — `"snake_case"`, `"camelCase"`, etc.).
    ///
    /// Defaults to whatever `#[serde(rename_all = "...")]` declares on
    /// the same enum. Override here only to break from the serde shape.
    #[darling(default)]
    rename_all: Option<String>,
}

#[derive(FromVariant)]
#[darling(attributes(lua), forward_attrs(doc))]
struct LuaTaggedClassVariant {
    ident: syn::Ident,
    attrs: Vec<Attribute>,
    fields: ast::Fields<LuaField>,
    /// Override the variant's Lua name (e.g. `"password_login"`).
    /// Wins over the container's `rename_all` strategy.
    #[darling(default)]
    rename: Option<String>,
}

pub(crate) fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let container = match LuaTaggedClassContainer::from_derive_input(&input) {
        Ok(c) => c,
        Err(e) => return e.write_errors().into(),
    };

    let ident = &container.ident;
    let class_alias = &container.class;
    // `#[lua(tag = ...)]` and `#[lua(rename_all = ...)]` take precedence;
    // otherwise pick the values up from `#[serde(...)]` so the user
    // doesn't have to mirror them.
    let (serde_tag, serde_rename_all) = extract_serde_meta(&container.attrs);
    let tag = container.tag.as_deref().or(serde_tag.as_deref());
    let rename_all = container
        .rename_all
        .as_deref()
        .or(serde_rename_all.as_deref());
    let enum_docs = extract_docs(&container.attrs);

    let Some(variants) = container.data.take_enum() else {
        unreachable!("supports(enum_any) guarantees this is an enum");
    };

    let mut emit_stmts: Vec<TokenStream2> = Vec::new();
    let mut variant_class_names: Vec<String> = Vec::with_capacity(variants.len());

    for v in &variants {
        let variant_lua_name = v
            .rename
            .clone()
            .unwrap_or_else(|| apply_rename_all(&v.ident.to_string(), rename_all));

        let variant_class = format!("{class_alias}{}", v.ident);
        variant_class_names.push(variant_class.clone());

        let variant_docs = extract_docs(&v.attrs);
        let header = build_class_header(&variant_class, None, &variant_docs);
        emit_stmts.push(quote! { out.push_str(#header); });

        // Discriminator first (when `tag` is set).
        if let Some(tag_name) = tag {
            let line = format!("--- @field {tag_name} \"{variant_lua_name}\"\n");
            emit_stmts.push(quote! { out.push_str(#line); });
        }

        // Variant's own fields.
        let field_stmts = match build_variant_fields(&v.fields) {
            Ok(stmts) => stmts,
            Err(e) => return e.write_errors().into(),
        };
        emit_stmts.extend(field_stmts);

        // Blank line between variants.
        emit_stmts.push(quote! { out.push('\n'); });
    }

    // Union alias — `--- @alias crap.X` followed by one `--- | crap.X<V>`
    // per variant.
    let mut alias_body = String::new();
    for line in &enum_docs {
        push_doc_line(&mut alias_body, line);
    }
    alias_body.push_str("--- @alias ");
    alias_body.push_str(class_alias);
    alias_body.push('\n');
    for vc in &variant_class_names {
        alias_body.push_str("--- | ");
        alias_body.push_str(vc);
        alias_body.push('\n');
    }
    alias_body.push('\n');
    emit_stmts.push(quote! { out.push_str(#alias_body); });

    let expanded = quote! {
        impl LuaAlias for #ident {
            const ALIAS_NAME: &'static str = #class_alias;

            fn render_lua_alias(out: &mut ::std::string::String) {
                #(#emit_stmts)*
            }
        }
    };

    expanded.into()
}

/// Pull `tag = "..."` and `rename_all = "..."` out of any
/// `#[serde(...)]` attributes on the container. Silently consumes
/// other serde keys (`untagged`, `deny_unknown_fields`, `default = "fn"`,
/// etc.) so `parse_nested_meta` can continue past them — those are
/// read directly by serde at compile time and don't need to be
/// mirrored here. Malformed `#[serde(...)]` attributes are silently
/// ignored: serde itself will produce the user-facing error at the
/// same site.
fn extract_serde_meta(attrs: &[Attribute]) -> (Option<String>, Option<String>) {
    let mut tag = None;
    let mut rename_all = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                let value: syn::LitStr = meta.value()?.parse()?;
                tag = Some(value.value());
                return Ok(());
            }
            if meta.path.is_ident("rename_all") {
                let value: syn::LitStr = meta.value()?.parse()?;
                rename_all = Some(value.value());
                return Ok(());
            }
            // Unknown key — consume its `= value` or `(…)` payload
            // (if any) so parsing can advance to the next comma.
            // Flag-only keys (`untagged`, `deny_unknown_fields`) need
            // no consumption.
            if meta.input.peek(syn::Token![=]) {
                let _: syn::Expr = meta.value()?.parse()?;
            } else if meta.input.peek(syn::token::Paren) {
                let _group: proc_macro2::Group = meta.input.parse()?;
            }
            Ok(())
        });
    }
    (tag, rename_all)
}

fn build_variant_fields(fields: &ast::Fields<LuaField>) -> darling::Result<Vec<TokenStream2>> {
    if fields.is_unit() {
        return Ok(Vec::new());
    }

    if matches!(fields.style, ast::Style::Tuple) {
        return Err(darling::Error::custom(
            "LuaTaggedClass: tuple variants are not supported; \
             use struct variants (e.g. `Variant { field: Type }`) so the macro \
             can name each field",
        ));
    }

    let mut out = Vec::with_capacity(fields.fields.len());
    for f in &fields.fields {
        if f.skip {
            continue;
        }
        let Some(ident) = &f.ident else {
            return Err(darling::Error::custom(
                "LuaTaggedClass: unnamed fields require explicit names \
                 (struct variants only)",
            ));
        };
        let lua_name = f.rename.clone().unwrap_or_else(|| ident.to_string());
        out.extend(build_field_emit(&lua_name, f)?);
    }
    Ok(out)
}
