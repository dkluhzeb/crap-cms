//! `#[derive(LuaAlias)]` — emit `--- @alias crap.X` from an enum.
//!
//! Three output styles are supported, auto-detected from variant shape:
//!
//! 1. **Literal-union** (all-unit-variant enums, e.g. `enum X { A, B }`)
//!    — emits a multi-line `--- @alias crap.X` block with one
//!    `--- | "name"  # doc` line per variant.
//! 2. **Type-union** (all-newtype-variant enums, e.g.
//!    `enum X { Plain(String), Localized(HashMap<String, String>) }`) —
//!    emits a single-line `--- @alias crap.X type1 | type2 | ...` with
//!    each variant's payload type auto-mapped.
//! 3. **Mixed** (unit + newtype, e.g.
//!    `enum X { Full, Half, Custom(String) }`) — emits a single-line
//!    `--- @alias crap.X "full" | "half" | string` with unit variants
//!    rendered as quoted literals (honoring `rename_all` / `#[lua(rename)]`)
//!    and newtype variants as their mapped type.

use darling::{FromDeriveInput, FromField, FromVariant, ast};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, DeriveInput, parse_macro_input};

use crate::shared::{apply_rename_all, extract_docs, push_doc_line};

#[derive(FromDeriveInput)]
#[darling(attributes(lua), supports(enum_any), forward_attrs(doc))]
struct LuaAliasContainer {
    ident: syn::Ident,
    data: ast::Data<LuaAliasVariant, ()>,
    attrs: Vec<Attribute>,
    alias: String,
    #[darling(default)]
    rename_all: Option<String>,
}

#[derive(FromVariant)]
#[darling(attributes(lua), forward_attrs(doc))]
struct LuaAliasVariant {
    ident: syn::Ident,
    attrs: Vec<Attribute>,
    #[darling(default)]
    rename: Option<String>,
    fields: ast::Fields<LuaAliasField>,
    /// Optional Lua class name for the per-field-type "view" subclass
    /// (e.g. `"crap.TextField"`). When ANY variant on the enum carries
    /// this attribute, the derive also emits an
    /// `impl LuaFieldTypeViewsDiscriminator` whose `VIEWS` table maps
    /// each variant's renamed slug → its view class. Variants without
    /// `view_class` are omitted from `VIEWS`. See the
    /// `LuaFieldTypeViews` derive for the consumer side.
    #[darling(default)]
    view_class: Option<String>,
}

#[derive(FromField)]
struct LuaAliasField {
    ty: syn::Type,
}

pub(crate) fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let container = match LuaAliasContainer::from_derive_input(&input) {
        Ok(c) => c,
        Err(e) => return e.write_errors().into(),
    };

    let ident = &container.ident;
    let alias = &container.alias;
    let rename_all = container.rename_all.as_deref();
    let enum_docs = extract_docs(&container.attrs);

    let Some(enum_variants) = container.data.take_enum() else {
        unreachable!("supports(enum_any) guarantees this is an enum");
    };

    let rendered_body = match render_alias_body(alias, &enum_docs, &enum_variants, rename_all) {
        Ok(s) => s,
        Err(e) => return e.write_errors().into(),
    };

    // If any variant carries `#[lua(view_class = "...")]`, additionally
    // emit an `impl LuaFieldTypeViewsDiscriminator` that pairs each
    // variant's renamed slug with its view class. Variants without
    // `view_class` are omitted from VIEWS — they signal "no per-type
    // view derived for this variant" (e.g. `FieldType::Join`, whose
    // Lua shape is a flat structure the derive can't model).
    let discriminator_impl = if enum_variants.iter().any(|v| v.view_class.is_some()) {
        let entries: Vec<TokenStream2> = enum_variants
            .iter()
            .filter_map(|v| {
                let class = v.view_class.as_ref()?;
                let slug = v
                    .rename
                    .clone()
                    .unwrap_or_else(|| apply_rename_all(&v.ident.to_string(), rename_all));
                Some(quote! { (#slug, #class) })
            })
            .collect();
        quote! {
            impl crate::typegen::lua::LuaFieldTypeViewsDiscriminator for #ident {
                const VIEWS: &'static [(&'static str, &'static str)] = &[
                    #(#entries),*
                ];
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        impl LuaAlias for #ident {
            const ALIAS_NAME: &'static str = #alias;

            fn render_lua_alias(out: &mut ::std::string::String) {
                out.push_str(#rendered_body);
            }
        }

        #discriminator_impl
    };

    expanded.into()
}

/// Render literal-union, type-union, or mixed-mode alias depending on
/// the variant shape.
///
/// Output order matches the variant declaration order in the source
/// enum (darling/syn populate `Data::take_enum()` in source order and
/// `map_type_to_string` is a deterministic AST walk — no `HashMap` /
/// `HashSet` iteration anywhere on this path). The full pipeline's
/// idempotency is exercised by
/// `static_file::tests::render_static_file_is_idempotent`.
fn render_alias_body(
    alias: &str,
    enum_docs: &[String],
    variants: &[LuaAliasVariant],
    rename_all: Option<&str>,
) -> darling::Result<String> {
    let has_payload = variants.iter().any(|v| !v.fields.is_empty());
    let has_unit = variants.iter().any(|v| v.fields.is_empty());

    if has_payload && has_unit {
        render_mixed_union(alias, enum_docs, variants, rename_all)
    } else if has_payload {
        render_type_union(alias, enum_docs, variants)
    } else {
        Ok(render_literal_union(alias, enum_docs, variants, rename_all))
    }
}

/// Mixed-mode output: unit variants become quoted literals, newtype
/// variants become their mapped types, all joined by ` | `.
fn render_mixed_union(
    alias: &str,
    enum_docs: &[String],
    variants: &[LuaAliasVariant],
    rename_all: Option<&str>,
) -> darling::Result<String> {
    let mut atoms: Vec<String> = Vec::with_capacity(variants.len());
    for v in variants {
        if v.fields.is_empty() {
            let lua_name = v
                .rename
                .clone()
                .unwrap_or_else(|| apply_rename_all(&v.ident.to_string(), rename_all));
            atoms.push(format!("\"{lua_name}\""));
        } else {
            let fields = &v.fields.fields;
            if fields.len() != 1 {
                return Err(darling::Error::custom(format!(
                    "LuaAlias mixed-union: payload variant `{}` must be a single-element tuple (newtype); got {} fields",
                    v.ident,
                    fields.len()
                )));
            }
            atoms.push(crate::lua_fn::map_type_to_string(&fields[0].ty)?);
        }
    }

    let mut s = String::new();
    for line in enum_docs {
        push_doc_line(&mut s, line);
    }
    s.push_str("--- @alias ");
    s.push_str(alias);
    s.push(' ');
    s.push_str(&atoms.join(" | "));
    s.push('\n');
    s.push('\n');
    Ok(s)
}

/// Type-union output for newtype-variant enums.
fn render_type_union(
    alias: &str,
    enum_docs: &[String],
    variants: &[LuaAliasVariant],
) -> darling::Result<String> {
    let mut types: Vec<String> = Vec::with_capacity(variants.len());
    for v in variants {
        let fields = &v.fields.fields;
        if fields.len() != 1 {
            return Err(darling::Error::custom(format!(
                "LuaAlias type-union: variant `{}` must be a single-element tuple variant (newtype); got {} fields",
                v.ident,
                fields.len()
            )));
        }
        types.push(crate::lua_fn::map_type_to_string(&fields[0].ty)?);
    }

    let mut s = String::new();
    for line in enum_docs {
        push_doc_line(&mut s, line);
    }
    s.push_str("--- @alias ");
    s.push_str(alias);
    s.push(' ');
    s.push_str(&types.join(" | "));
    s.push('\n');
    s.push('\n');
    Ok(s)
}

/// Literal-union output for unit-variant enums.
fn render_literal_union(
    alias: &str,
    enum_docs: &[String],
    variants: &[LuaAliasVariant],
    rename_all: Option<&str>,
) -> String {
    let mut s = String::new();

    for line in enum_docs {
        push_doc_line(&mut s, line);
    }
    s.push_str("--- @alias ");
    s.push_str(alias);
    s.push('\n');

    // Pre-compute all variant Lua names + their doc strings so we can
    // column-align the trailing `# description` (matches the convention
    // in the hand-written `types/crap.lua`).
    let entries: Vec<(String, String)> = variants
        .iter()
        .map(|v| {
            let lua_name = v
                .rename
                .clone()
                .unwrap_or_else(|| apply_rename_all(&v.ident.to_string(), rename_all));
            let docs = extract_docs(&v.attrs);
            let joined: String = docs
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (lua_name, joined)
        })
        .collect();

    // Width of `"<name>"` for each variant; pad shorter variants so all
    // `# description` columns line up. Only useful when at least one
    // variant has a doc — otherwise emit compact form.
    let any_doc = entries.iter().any(|(_, d)| !d.is_empty());
    let max_width = entries
        .iter()
        .map(|(name, _)| name.len() + 2) // +2 for surrounding quotes
        .max()
        .unwrap_or(0);

    for (lua_name, doc) in &entries {
        s.push_str("--- | \"");
        s.push_str(lua_name);
        s.push('"');

        if !doc.is_empty() {
            // Pad to align all `#` markers at the same column.
            let written = lua_name.len() + 2; // `"<name>"`
            let pad = max_width.saturating_sub(written);
            for _ in 0..pad {
                s.push(' ');
            }
            s.push_str(" # ");
            s.push_str(doc);
        } else if any_doc {
            // Keep alignment-friendly trailing whitespace off when the
            // variant has no doc — no trailing-whitespace lint hits.
        }
        s.push('\n');
    }
    s.push('\n');
    s
}
