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
//!
//! With `layout_base = "crap.LayoutField"` and `layout_views = "row, …"`
//! on the container, the common fields marked `#[lua(layout)]` move to a
//! `--- @class crap.LayoutField` block emitted first, the base class
//! extends it, and the views listed in `layout_views` extend it instead of
//! the base class — so a layout wrapper's view offers only the keys a
//! wrapper accepts. `layout_doc` is that block's doc line.
//!
//! `virtual_base` / `virtual_views` / `virtual_doc` add one more tier between
//! the two, for a field with no stored value (a join): the common fields
//! marked `#[lua(virtual_field)]` move to that class, which extends the
//! layout base, and the base class extends it instead. The chain is
//! `LayoutField ← VirtualField ← BaseField`, each class holding only the keys
//! the narrower one lacks.

use std::collections::BTreeMap;

use darling::{FromDeriveInput, ast};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, DeriveInput, parse_macro_input};

use crate::shared::{
    LuaField, apply_rename_all, build_class_header, build_field_emit, extract_docs,
    from_derive_input_or_return, strip_option,
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
    /// The layout-wrapper base class (e.g. `"crap.LayoutField"`): holds the
    /// common fields marked `#[lua(layout)]`; `base` extends it.
    #[darling(default)]
    layout_base: Option<String>,
    /// `"row, collapsible, tabs"` — the discriminator slugs whose views
    /// extend `layout_base` instead of `base`.
    #[darling(default)]
    layout_views: Option<String>,
    /// Doc line of the `layout_base` class block.
    #[darling(default)]
    layout_doc: Option<String>,
    /// The virtual-field base class (e.g. `"crap.VirtualField"`): holds the
    /// common fields marked `#[lua(virtual_field)]`; extends `layout_base`,
    /// and `base` extends it.
    #[darling(default)]
    virtual_base: Option<String>,
    /// `"join"` — the discriminator slugs whose views extend `virtual_base`
    /// instead of `base`.
    #[darling(default)]
    virtual_views: Option<String>,
    /// Doc line of the `virtual_base` class block.
    #[darling(default)]
    virtual_doc: Option<String>,
}

/// The `--- @field` statements of every non-skipped field `keep` selects.
fn field_stmts(
    fields: &[LuaField],
    rename_all: Option<&str>,
    keep: impl Fn(&LuaField) -> bool,
) -> darling::Result<Vec<TokenStream2>> {
    let mut stmts = Vec::new();

    for f in fields.iter().filter(|f| !f.skip && keep(f)) {
        let Some(name) = f.ident.as_ref() else {
            continue;
        };

        let lua_name = f
            .rename
            .clone()
            .unwrap_or_else(|| apply_rename_all(&name.to_string(), rename_all));

        stmts.extend(build_field_emit(&lua_name, f)?);
    }

    Ok(stmts)
}

/// One narrowed base class the container declares (`layout_base` or
/// `virtual_base`), borrowed apart from its `data`.
struct TierAttrs<'a> {
    /// The class name; `None` when the container declares no such tier.
    class: Option<&'a str>,
    /// The class it extends (the next narrower tier), if any.
    parent: Option<&'a str>,
    views: Option<&'a str>,
    doc: Option<&'a str>,
    /// The attribute names, for the error when the tier is half-declared.
    attrs: &'static str,
}

/// A tier's class block (header + the fields `keep` selects) and the view
/// slugs that extend it; empty when the container declares no such tier.
fn tier_block(
    tier: &TierAttrs<'_>,
    rename_all: Option<&str>,
    fields: &[LuaField],
    keep: impl Fn(&LuaField) -> bool,
) -> darling::Result<(TokenStream2, Vec<String>)> {
    let Some(class) = tier.class else {
        if tier.views.is_some() || fields.iter().any(&keep) {
            return Err(darling::Error::custom(format!(
                "{} need their container base class",
                tier.attrs
            )));
        }

        return Ok((TokenStream2::new(), Vec::new()));
    };

    let docs: Vec<String> = tier.doc.iter().map(ToString::to_string).collect();
    let header = build_class_header(class, tier.parent, &docs);
    let stmts = field_stmts(fields, rename_all, |f| keep(f) && f.applies_to.is_none())?;

    let slugs = tier
        .views
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    let block = quote! {
        out.push_str(#header);
        #(#stmts)*
        out.push('\n');
    };

    Ok((block, slugs))
}

/// Bucket the fields by `applies_to` slug and emit one match arm per unique
/// slug, each writing that view's `--- @field` lines.
fn view_match_arms(
    fields: &[LuaField],
    rename_all: Option<&str>,
) -> darling::Result<Vec<TokenStream2>> {
    let mut slug_to_fields: BTreeMap<String, Vec<&LuaField>> = BTreeMap::new();
    for f in fields {
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

    let mut match_arms = Vec::new();
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
                .unwrap_or_else(|| apply_rename_all(&name.to_string(), rename_all));
            arm_stmts.extend(build_field_emit(&lua_name, f)?);
        }
        match_arms.push(quote! {
            #slug => { #(#arm_stmts)* }
        });
    }

    Ok(match_arms)
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

    // ── LayoutField (optional): the common fields a layout wrapper accepts.
    let layout_class = container.layout_base.as_deref();
    let layout = TierAttrs {
        class: layout_class,
        parent: None,
        views: container.layout_views.as_deref(),
        doc: container.layout_doc.as_deref(),
        attrs: "`layout_views` / `#[lua(layout)]`",
    };
    let (layout_block, layout_slugs) = match tier_block(&layout, rename_all, &fields, |f| f.layout)
    {
        Ok(block) => block,
        Err(e) => return e.write_errors().into(),
    };

    // ── VirtualField (optional): the further common fields a virtual field
    //    (no stored value) accepts; extends the layout base.
    let virtual_class = container.virtual_base.as_deref();
    let virtual_tier = TierAttrs {
        class: virtual_class,
        parent: layout_class,
        views: container.virtual_views.as_deref(),
        doc: container.virtual_doc.as_deref(),
        attrs: "`virtual_views` / `#[lua(virtual_field)]`",
    };
    let (virtual_block, virtual_slugs) =
        match tier_block(&virtual_tier, rename_all, &fields, |f| f.virtual_field) {
            Ok(block) => block,
            Err(e) => return e.write_errors().into(),
        };

    // ── BaseField: every other field with no `applies_to` (and not `skip`),
    //    extending the nearest declared tier.
    let base_parent = virtual_class.or(layout_class);
    let base_header = build_class_header(base_class, base_parent, &struct_docs);
    let base_stmts = match field_stmts(&fields, rename_all, |f| {
        f.applies_to.is_none()
            && !(f.layout && layout_class.is_some())
            && !(f.virtual_field && virtual_class.is_some())
    }) {
        Ok(stmts) => stmts,
        Err(e) => return e.write_errors().into(),
    };
    let layout_parent = layout_class.unwrap_or(base_class.as_str());
    let virtual_parent = virtual_class.unwrap_or(base_class.as_str());

    // ── Per-view: one match arm per `applies_to` slug. At runtime, iterate
    //    the discriminator's VIEWS table and dispatch on each slug.
    let match_arms = match view_match_arms(&fields, rename_all) {
        Ok(arms) => arms,
        Err(e) => return e.write_errors().into(),
    };

    let expanded = quote! {
        impl LuaFieldTypeViews for #ident {
            fn render_lua_field_type_views(out: &mut ::std::string::String) {
                // The views that extend a narrowed base instead of the base.
                const LAYOUT_VIEWS: &[&str] = &[#(#layout_slugs),*];
                const VIRTUAL_VIEWS: &[&str] = &[#(#virtual_slugs),*];

                // 1. Narrowed base blocks (when declared) — the common fields a
                //    layout wrapper accepts, then those a virtual field adds.
                #layout_block
                #virtual_block

                // 2. Base class block — all (other) common fields.
                out.push_str(#base_header);
                #(#base_stmts)*
                out.push('\n');

                // 3. Per-variant subclasses — driven by the discriminator's
                //    `VIEWS` table at runtime. Fully-qualified path so
                //    users don't need to import
                //    `LuaFieldTypeViewsDiscriminator`.
                for (slug, class)
                    in <#discriminator as crate::typegen::lua::LuaFieldTypeViewsDiscriminator>::VIEWS
                {
                    let parent = if LAYOUT_VIEWS.contains(slug) {
                        #layout_parent
                    } else if VIRTUAL_VIEWS.contains(slug) {
                        #virtual_parent
                    } else {
                        #base_class
                    };

                    out.push_str("--- @class ");
                    out.push_str(class);
                    out.push_str(" : ");
                    out.push_str(parent);
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
