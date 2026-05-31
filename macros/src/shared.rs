//! Cross-derive helpers used by `LuaAnnotation`, `LuaAlias`,
//! `LuaTypeAlias`, and `LuaFieldTypeViews`. Field-list emission,
//! doc-comment extraction, Rust↔Lua type mapping, and identifier
//! renaming all live here so each derive module can stay focused on
//! its own input/output shape.

use darling::FromField;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Expr, Type, parse_str};

// ── Field input ──────────────────────────────────────────────────────

/// One named field of a struct that derives `LuaAnnotation` or
/// `LuaFieldTypeViews`. Both derives consume the same attribute set so
/// users don't have to remember which attrs each accepts.
#[derive(FromField)]
#[darling(attributes(lua), forward_attrs(doc))]
pub(crate) struct LuaField {
    pub(crate) ident: Option<syn::Ident>,
    pub(crate) ty: Type,
    pub(crate) attrs: Vec<Attribute>,

    #[darling(default)]
    pub(crate) optional: bool,

    #[darling(default, rename = "ty")]
    pub(crate) lua_ty: Option<String>,

    #[darling(default)]
    pub(crate) ty_expr: Option<String>,

    #[darling(default)]
    pub(crate) rename: Option<String>,

    #[darling(default)]
    pub(crate) skip: bool,

    /// `applies_to = "text, textarea"` — used only by `LuaFieldTypeViews`.
    /// Ignored by `LuaAnnotation`. When absent, the field is "common" and
    /// goes into the base class. When present, the field appears only in
    /// per-view subclasses whose slug is in this list.
    #[darling(default)]
    pub(crate) applies_to: Option<String>,

    /// `flatten` — used only by `LuaFieldTypeViews`. Ignored by
    /// `LuaAnnotation`. When set on a field whose inner type implements
    /// `LuaFieldBlock` (auto-derived alongside `LuaAnnotation`), the
    /// macro emits `<inner_ty as LuaFieldBlock>::render_lua_fields_only`
    /// in place of the normal `--- @field name? inner_ty` line.
    /// Combine with `applies_to` to flatten only in matching views.
    #[darling(default)]
    pub(crate) flatten: bool,
}

// ── Doc-comment extraction ───────────────────────────────────────────

/// Pull `#[doc = "..."]` attributes off an attribute list and return the
/// raw strings in source order. Trims a single leading space per line
/// (matches the convention `/// foo` → `" foo"` → `"foo"`).
pub(crate) fn extract_docs(attrs: &[Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter_map(|attr| {
            if !attr.path().is_ident("doc") {
                return None;
            }
            let syn::Meta::NameValue(nv) = &attr.meta else {
                return None;
            };
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            else {
                return None;
            };
            let raw = s.value();
            // /// foo → " foo" — strip exactly one leading space if present.
            let trimmed = raw.strip_prefix(' ').unwrap_or(&raw).to_string();
            Some(trimmed)
        })
        .collect()
}

/// Append one doc-comment line as Lua prose. Empty lines emit as `---\n`
/// (no trailing space — avoids trailing-whitespace lint hits in output).
/// Non-empty lines emit as `--- {line}\n`, restoring the leading space
/// that `extract_docs` strips off `/// foo` → `" foo"` → `"foo"`.
pub(crate) fn push_doc_line(out: &mut String, line: &str) {
    if line.is_empty() {
        out.push_str("---\n");
    } else {
        out.push_str("--- ");
        out.push_str(line);
        out.push('\n');
    }
}

/// Compose the `--- @class` header block: leading doc-comment prose +
/// the class declaration line (optionally with `: extends`).
pub(crate) fn build_class_header(class: &str, extends: Option<&str>, docs: &[String]) -> String {
    let mut s = String::new();
    for line in docs {
        push_doc_line(&mut s, line);
    }
    s.push_str("--- @class ");
    s.push_str(class);
    if let Some(parent) = extends {
        s.push_str(" : ");
        s.push_str(parent);
    }
    s.push('\n');
    s
}

// ── Field emission (`--- @field` line per field) ─────────────────────

/// Build the statements that append a single `--- @field` line for one
/// field. Shared by `LuaAnnotation`'s base path and
/// `LuaFieldTypeViews`'s non-flatten per-view path.
pub(crate) fn build_field_emit(name: &str, f: &LuaField) -> darling::Result<Vec<TokenStream2>> {
    let (inferred_optional, inner_ty) = strip_option(&f.ty);
    let is_optional = f.optional || inferred_optional;
    let opt_marker = if is_optional { "?" } else { "" };

    let doc_suffix = build_field_doc_suffix(&f.attrs);
    let prefix = format!("--- @field {name}{opt_marker} ");

    // `ty` and `ty_expr` both set is almost always a copy-paste bug — one
    // is a compile-time literal, the other a runtime expression — and
    // silently preferring `ty` masks the typo. Error out so the user
    // picks one explicitly.
    if f.lua_ty.is_some() && f.ty_expr.is_some() {
        return Err(darling::Error::custom(format!(
            "field `{name}`: `#[lua(ty = \"...\")]` and `#[lua(ty_expr = \"...\")]` are mutually exclusive — pick one"
        )));
    }

    if let Some(lit) = &f.lua_ty {
        let line = format!("{prefix}{lit}{doc_suffix}\n");
        return Ok(vec![quote! { out.push_str(#line); }]);
    }

    if let Some(expr_src) = &f.ty_expr {
        let expr: Expr = parse_str(expr_src)
            .map_err(|e| darling::Error::custom(format!("invalid ty_expr `{expr_src}`: {e}")))?;
        let trailing = format!("{doc_suffix}\n");
        return Ok(vec![
            quote! { out.push_str(#prefix); },
            quote! { out.push_str(&{ #expr }); },
            quote! { out.push_str(#trailing); },
        ]);
    }

    match map_type(inner_ty)? {
        TypeMapping::Literal(s) => {
            let line = format!("{prefix}{s}{doc_suffix}\n");
            Ok(vec![quote! { out.push_str(#line); }])
        }
        TypeMapping::ClassRef(ty, suffix) => {
            let trailing = format!("{suffix}{doc_suffix}\n");
            Ok(vec![
                quote! { out.push_str(#prefix); },
                quote! { out.push_str(<#ty as LuaAnnotation>::CLASS_NAME); },
                quote! { out.push_str(#trailing); },
            ])
        }
    }
}

/// Render trailing prose for a `--- @field` line from the field's
/// doc-comments. Joins multi-line docs with single spaces (`LuaLS`
/// treats the rest of the line as the field's description). Returns
/// `""` when there are no docs.
fn build_field_doc_suffix(attrs: &[Attribute]) -> String {
    let docs = extract_docs(attrs);
    if docs.is_empty() {
        return String::new();
    }
    let joined: String = docs
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        String::new()
    } else {
        format!(" {joined}")
    }
}

// ── Type mapping ─────────────────────────────────────────────────────

pub(crate) enum TypeMapping {
    /// Resolved entirely at compile time — e.g., `"string"`, `"boolean"`,
    /// `"string[]"`.
    Literal(String),
    /// References another `LuaAnnotation` type — resolve via
    /// `<T as LuaAnnotation>::CLASS_NAME` at runtime. The `&'static str`
    /// suffix is appended after the class name (`""` for direct ref,
    /// `"[]"` for `Vec<T>`). `Type` boxed to keep the enum compact —
    /// `syn::Type` is ~240 bytes, dwarfing the other variant.
    ClassRef(Box<Type>, &'static str),
}

pub(crate) fn map_type(ty: &Type) -> darling::Result<TypeMapping> {
    let ty = strip_ref(ty);

    if let Some(inner) = unwrap_path("Vec", ty) {
        return match map_scalar(inner)? {
            ScalarMap::Literal(s) => Ok(TypeMapping::Literal(format!("{s}[]"))),
            ScalarMap::ClassRef(t) => Ok(TypeMapping::ClassRef(Box::new(t), "[]")),
        };
    }

    match map_scalar(ty)? {
        ScalarMap::Literal(s) => Ok(TypeMapping::Literal(s)),
        ScalarMap::ClassRef(t) => Ok(TypeMapping::ClassRef(Box::new(t), "")),
    }
}

enum ScalarMap {
    Literal(String),
    ClassRef(Type),
}

fn map_scalar(ty: &Type) -> darling::Result<ScalarMap> {
    let ty = strip_ref(ty);
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        let name = seg.ident.to_string();
        return Ok(match name.as_str() {
            "String" | "str" => ScalarMap::Literal("string".into()),
            "bool" => ScalarMap::Literal("boolean".into()),
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64"
            | "i128" | "isize" => ScalarMap::Literal("integer".into()),
            "f32" | "f64" => ScalarMap::Literal("number".into()),
            _ => ScalarMap::ClassRef(ty.clone()),
        });
    }
    Err(darling::Error::custom(format!(
        "cannot auto-map field type `{}`; add #[lua(ty = \"...\")] or #[lua(ty_expr = \"...\")]",
        quote!(#ty)
    )))
}

/// Returns `(true, inner)` if `ty` is `Option<T>`, else `(false, ty)`.
pub(crate) fn strip_option(ty: &Type) -> (bool, &Type) {
    match unwrap_path("Option", ty) {
        Some(inner) => (true, inner),
        None => (false, ty),
    }
}

pub(crate) fn strip_ref(ty: &Type) -> &Type {
    if let Type::Reference(r) = ty {
        return strip_ref(&r.elem);
    }
    ty
}

/// If `ty` is a path whose last segment is `target<T, ...>`, returns
/// `Some(T)`.
pub(crate) fn unwrap_path<'a>(target: &str, ty: &'a Type) -> Option<&'a Type> {
    let Type::Path(p) = ty else { return None };
    let seg = p.path.segments.last()?;
    if seg.ident != target {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let first = args.args.first()?;
    let syn::GenericArgument::Type(t) = first else {
        return None;
    };
    Some(t)
}

// ── Identifier renaming (rename_all strategies) ──────────────────────

/// Apply a `rename_all` strategy to a Rust enum variant identifier.
///
/// Mirrors serde's strategies (subset): `snake_case`, `lowercase`,
/// `camelCase`, `kebab-case`. Unknown strategies leave the name
/// unchanged.
pub(crate) fn apply_rename_all(variant: &str, strategy: Option<&str>) -> String {
    match strategy {
        Some("snake_case") => to_snake_case(variant),
        Some("lowercase") => variant.to_lowercase(),
        Some("camelCase") => to_camel_case(variant),
        Some("kebab-case") => to_snake_case(variant).replace('_', "-"),
        _ => variant.to_string(),
    }
}

fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// `PascalCase` or `snake_case` → `camelCase`.
///
/// `FooBar` → `fooBar` (first char lowercased, rest passed through).
/// `foo_bar` → `fooBar` (underscores consumed, following char uppercased).
fn to_camel_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return out;
    };
    out.push(first.to_ascii_lowercase());

    let mut upper_next = false;
    for c in chars {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_snake_case_inserts_underscores_before_inner_uppercase() {
        assert_eq!(to_snake_case("FooBar"), "foo_bar");
        assert_eq!(to_snake_case("foo"), "foo");
        assert_eq!(to_snake_case("ABC"), "a_b_c");
        // Leading uppercase is lowercased but not prefixed with `_`.
        assert_eq!(to_snake_case("Foo"), "foo");
        assert_eq!(to_snake_case(""), "");
    }

    #[test]
    fn to_camel_case_handles_pascal_and_snake_inputs() {
        assert_eq!(to_camel_case("FooBar"), "fooBar");
        assert_eq!(to_camel_case("foo_bar"), "fooBar");
        assert_eq!(to_camel_case("foo_bar_baz"), "fooBarBaz");
        assert_eq!(to_camel_case("foo"), "foo");
        assert_eq!(to_camel_case(""), "");
    }

    #[test]
    fn apply_rename_all_dispatches_each_strategy() {
        assert_eq!(apply_rename_all("FooBar", Some("snake_case")), "foo_bar");
        assert_eq!(apply_rename_all("FooBar", Some("lowercase")), "foobar");
        assert_eq!(apply_rename_all("foo_bar", Some("camelCase")), "fooBar");
        assert_eq!(apply_rename_all("FooBar", Some("kebab-case")), "foo-bar");
        // Unknown strategy and no strategy both pass the name through.
        assert_eq!(apply_rename_all("FooBar", Some("???")), "FooBar");
        assert_eq!(apply_rename_all("FooBar", None), "FooBar");
    }

    #[test]
    fn push_doc_line_blank_vs_text() {
        let mut s = String::new();
        push_doc_line(&mut s, "");
        push_doc_line(&mut s, "hello");
        assert_eq!(s, "---\n--- hello\n");
    }

    #[test]
    fn build_class_header_composes_docs_class_and_extends() {
        assert_eq!(
            build_class_header("crap.Foo", None, &["Doc line".to_string()]),
            "--- Doc line\n--- @class crap.Foo\n"
        );
        assert_eq!(
            build_class_header("crap.Foo", Some("crap.Base"), &[]),
            "--- @class crap.Foo : crap.Base\n"
        );
    }
}
