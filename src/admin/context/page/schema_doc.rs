//! Auto-generated reference doc for the typed admin page contexts.
//!
//! Walks every per-page typed context (login, dashboard, collection edit,
//! …) and produces the Markdown content at
//! `docs/src/admin-ui/reference/template-context.md` via
//! [`generate_template_context_md`]. The xtask
//! `cargo xtask gen-template-doc [--check]` writes / diffs that file; an
//! in-crate `#[test]` at the bottom of this file also asserts drift so
//! local `cargo test` runs catch staleness without needing the xtask.
//!
//! The Markdown structure lives in an inline Handlebars template (we already
//! depend on the engine for admin pages, so reusing it keeps the doc layout
//! editable as a template and out of Rust string concat). The Rust side
//! preprocesses each per-page schema into a flat `DocContext` struct that
//! the template renders.

use std::sync::LazyLock;

use handlebars::Handlebars;
use regex::Regex;
use schemars::{Schema, schema_for};
use serde::Serialize;
use serde_json::Value;

use crate::admin::custom_pages::CustomPage as RegisteredCustomPage;
use crate::core::{ConditionExpr, ConditionRow, DocumentFields};

use crate::admin::context::{
    AdminMeta, AuthBasePageContext, AuthMeta, BasePageContext, Breadcrumb, CollectionContext,
    CollectionPermissions, CrapMeta, DocumentRef, EditorLocaleOption, FieldAdminMeta, FieldContext,
    FieldMeta, GlobalContext, GlobalPermissions, NavData, PageMeta, PaginationContext, UploadMeta,
    UserContext, VersionsMeta,
    field::{
        ArrayField, ArrayRow, BaseFieldData, BlockDefinition, BlockRow, BlocksField, CheckboxField,
        ChoiceField, CodeField, ConditionData, DateField, GroupField, JoinField, JoinItem,
        NumberField, RelationshipField, RelationshipSelectedItem, RichtextField,
        RichtextNodeAttrCtx, RichtextNodeAttrOption, RichtextNodeDefCtx, RowField, SelectOption,
        TabPanel, TabsField, TextField, TextareaField, TimezoneOption, UploadField,
        ValidationAttrs,
    },
    locale_template::LocaleTemplateOption,
    nav::{NavCollection, NavGlobal},
    page::{
        auth::{AuthCollection, ForgotPasswordPage, LoginPage, MfaPage, ResetPasswordPage},
        collections::{
            CollectionCreatePage, CollectionDeleteConfirmPage, CollectionEditPage, CollectionEntry,
            CollectionFormErrorPage, CollectionItemsListPage, CollectionListPage,
            CollectionRestoreConfirmPage, CollectionVersionsListPage, UploadFormContext,
            UploadInfo,
        },
        custom::CustomPage,
        dashboard::{CollectionCard, DashboardPage, GlobalCard},
        errors::ErrorPage,
        globals::{
            GlobalEditPage, GlobalFormErrorPage, GlobalRestoreConfirmPage, GlobalVersionsListPage,
        },
    },
};

// ── Handlebars template ─────────────────────────────────────────────

/// Inline Handlebars template that produces the Markdown doc. Whitespace
/// outside `{{ }}` blocks is significant — keep the structure tight so the
/// rendered output stays clean.
const TEMPLATE: &str = r"<!--
  AUTO-GENERATED — do not edit by hand.
  Source of truth: typed page-context structs in `src/admin/context/page/`.
  Regenerate with: `cargo xtask gen-template-doc`
-->

# Admin template context reference

Every admin page renders a typed Rust struct serialized to JSON, runs it through the optional [`before_render`](../../hooks/lifecycle-events.md#before_render) Lua hook, and hands it to Handlebars. This file lists every page, its `page.type` discriminant, the template it renders, and the fields the template can rely on. The `page.type` column doubles as the `info.page` value a `before_render` hook receives, and the template column as `info.template`.

Field types use Rust-style notation: `string`, `integer`, `boolean`, `Vec<T>`, `Option<T>`. Composite leaves like `CrapMeta`, `NavData`, `FieldContext` link into the [shared definitions](#shared-definitions) section at the bottom.

{{#each pages}}
## {{heading}}

- **`page.type`**: `{{page_type}}`
- **Template**: `templates/{{template}}.hbs`

{{#if fields}}
{{#each fields}}
- **`{{name}}`** ({{{ty}}}){{#unless required}} _(optional)_{{/unless}}{{#if description}} — {{description}}{{/if}}
{{/each}}
{{else}}
_(No fields.)_
{{/if}}

{{/each}}

---

## Shared definitions

Every page above flattens [BasePageContext](#basepagecontext) (or [AuthBasePageContext](#authbasepagecontext) for auth-flow pages) into its top-level fields. The base structs and their leaves are defined here once.

{{#each definitions}}
### {{name}}

{{#if description}}
{{description}}

{{/if}}
{{#if fields}}
{{#each fields}}
- **`{{name}}`** ({{{ty}}}){{#unless required}} _(optional)_{{/unless}}{{#if description}} — {{description}}{{/if}}
{{/each}}
{{else}}
{{#if enum_note}}
{{enum_note}}
{{else}}
_(No fields.)_
{{/if}}
{{/if}}

{{/each}}
";

// ── Doc-context shape ──────────────────────────────────────────────

#[derive(Serialize)]
struct DocContext {
    pages: Vec<PageDoc>,
    definitions: Vec<TypeDoc>,
}

#[derive(Serialize)]
struct PageDoc {
    heading: &'static str,
    page_type: &'static str,
    template: &'static str,
    fields: Vec<FieldDoc>,
}

#[derive(Serialize)]
struct TypeDoc {
    name: &'static str,
    /// Type-level doc comment from the Rust struct/enum (may be empty).
    description: String,
    /// For property-less enum schemas: a rendered one-liner listing the
    /// variant discriminators (empty for plain structs).
    enum_note: String,
    fields: Vec<FieldDoc>,
}

#[derive(Serialize)]
struct FieldDoc {
    name: String,
    /// Rendered type string — already markdown (may contain links). Used in
    /// the template via `{{{ty}}}` (triple-stash, no HTML-escape).
    ty: String,
    required: bool,
    description: String,
}

// ── Page-entry table ────────────────────────────────────────────────

struct PageEntry {
    heading: &'static str,
    page_type: &'static str,
    template: &'static str,
    schema: fn() -> Schema,
}

const AUTH_PAGES: &[PageEntry] = &[
    PageEntry {
        heading: "Login page",
        page_type: "auth_login",
        template: "auth/login",
        schema: || schema_for!(LoginPage),
    },
    PageEntry {
        heading: "MFA challenge page",
        page_type: "auth_mfa",
        template: "auth/mfa",
        schema: || schema_for!(MfaPage),
    },
    PageEntry {
        heading: "Forgot password page",
        page_type: "auth_forgot",
        template: "auth/forgot_password",
        schema: || schema_for!(ForgotPasswordPage),
    },
    PageEntry {
        heading: "Reset password page",
        page_type: "auth_reset",
        template: "auth/reset_password",
        schema: || schema_for!(ResetPasswordPage),
    },
];

const ERROR_PAGES: &[PageEntry] = &[PageEntry {
    heading: "Error pages (400 / 403 / 404 / 500)",
    page_type: "error_400 | error_403 | error_404 | error_500",
    template: "errors/{400,403,404,500}",
    schema: || schema_for!(ErrorPage),
}];

const DASHBOARD_PAGES: &[PageEntry] = &[PageEntry {
    heading: "Dashboard",
    page_type: "dashboard",
    template: "dashboard/index",
    schema: || schema_for!(DashboardPage),
}];

const COLLECTION_PAGES: &[PageEntry] = &[
    PageEntry {
        heading: "Collection list",
        page_type: "collection_list",
        template: "collections/list",
        schema: || schema_for!(CollectionListPage),
    },
    PageEntry {
        heading: "Collection items list",
        page_type: "collection_items",
        template: "collections/items",
        schema: || schema_for!(CollectionItemsListPage),
    },
    PageEntry {
        heading: "Collection edit form",
        page_type: "collection_edit",
        template: "collections/edit",
        schema: || schema_for!(CollectionEditPage),
    },
    PageEntry {
        heading: "Collection create form",
        page_type: "collection_create",
        template: "collections/edit",
        schema: || schema_for!(CollectionCreatePage),
    },
    PageEntry {
        heading: "Collection form-error re-render",
        page_type: "collection_edit | collection_create",
        template: "collections/edit",
        schema: || schema_for!(CollectionFormErrorPage),
    },
    PageEntry {
        heading: "Collection delete confirmation",
        page_type: "collection_delete",
        template: "collections/delete",
        schema: || schema_for!(CollectionDeleteConfirmPage),
    },
    PageEntry {
        heading: "Collection versions list",
        page_type: "collection_versions",
        template: "collections/versions",
        schema: || schema_for!(CollectionVersionsListPage),
    },
    PageEntry {
        heading: "Collection restore confirmation",
        page_type: "collection_versions",
        template: "collections/restore",
        schema: || schema_for!(CollectionRestoreConfirmPage),
    },
];

const GLOBAL_PAGES: &[PageEntry] = &[
    PageEntry {
        heading: "Global edit form",
        page_type: "global_edit",
        template: "globals/edit",
        schema: || schema_for!(GlobalEditPage),
    },
    PageEntry {
        heading: "Global form-error re-render",
        page_type: "global_edit",
        template: "globals/edit",
        schema: || schema_for!(GlobalFormErrorPage),
    },
    PageEntry {
        heading: "Global versions list",
        page_type: "global_versions",
        template: "globals/versions",
        schema: || schema_for!(GlobalVersionsListPage),
    },
    PageEntry {
        heading: "Global restore confirmation",
        page_type: "global_versions",
        template: "globals/restore",
        schema: || schema_for!(GlobalRestoreConfirmPage),
    },
];

const CUSTOM_PAGES: &[PageEntry] = &[PageEntry {
    heading: "Custom admin page",
    page_type: "custom_page",
    template: "pages/{slug}",
    schema: || schema_for!(CustomPage),
}];

fn pages() -> impl Iterator<Item = &'static PageEntry> {
    AUTH_PAGES
        .iter()
        .chain(ERROR_PAGES)
        .chain(DASHBOARD_PAGES)
        .chain(COLLECTION_PAGES)
        .chain(GLOBAL_PAGES)
        .chain(CUSTOM_PAGES)
}

fn definitions() -> Vec<(&'static str, Schema)> {
    vec![
        ("BasePageContext", schema_for!(BasePageContext)),
        ("AuthBasePageContext", schema_for!(AuthBasePageContext)),
        ("PageMeta", schema_for!(PageMeta)),
        ("CrapMeta", schema_for!(CrapMeta)),
        ("NavData", schema_for!(NavData)),
        ("NavCollection", schema_for!(NavCollection)),
        ("NavGlobal", schema_for!(NavGlobal)),
        ("UserContext", schema_for!(UserContext)),
        ("EditorLocaleOption", schema_for!(EditorLocaleOption)),
        ("LocaleTemplateOption", schema_for!(LocaleTemplateOption)),
        ("Breadcrumb", schema_for!(Breadcrumb)),
        ("CollectionContext", schema_for!(CollectionContext)),
        ("CollectionPermissions", schema_for!(CollectionPermissions)),
        ("AdminMeta", schema_for!(AdminMeta)),
        ("AuthMeta", schema_for!(AuthMeta)),
        ("UploadMeta", schema_for!(UploadMeta)),
        ("VersionsMeta", schema_for!(VersionsMeta)),
        ("FieldMeta", schema_for!(FieldMeta)),
        ("FieldAdminMeta", schema_for!(FieldAdminMeta)),
        ("GlobalContext", schema_for!(GlobalContext)),
        ("GlobalPermissions", schema_for!(GlobalPermissions)),
        ("DocumentRef", schema_for!(DocumentRef)),
        ("DocumentFields", schema_for!(DocumentFields)),
        ("ConditionExpr", schema_for!(ConditionExpr)),
        ("ConditionRow", schema_for!(ConditionRow)),
        ("TimezoneOption", schema_for!(TimezoneOption)),
        ("CustomPage", schema_for!(RegisteredCustomPage)),
        ("PaginationContext", schema_for!(PaginationContext)),
        ("FieldContext", schema_for!(FieldContext)),
        ("BaseFieldData", schema_for!(BaseFieldData)),
        ("ValidationAttrs", schema_for!(ValidationAttrs)),
        ("ConditionData", schema_for!(ConditionData)),
        ("TextField", schema_for!(TextField)),
        ("TextareaField", schema_for!(TextareaField)),
        ("NumberField", schema_for!(NumberField)),
        ("CodeField", schema_for!(CodeField)),
        ("RichtextField", schema_for!(RichtextField)),
        ("RichtextNodeDefCtx", schema_for!(RichtextNodeDefCtx)),
        ("RichtextNodeAttrCtx", schema_for!(RichtextNodeAttrCtx)),
        (
            "RichtextNodeAttrOption",
            schema_for!(RichtextNodeAttrOption),
        ),
        ("DateField", schema_for!(DateField)),
        ("CheckboxField", schema_for!(CheckboxField)),
        ("ChoiceField", schema_for!(ChoiceField)),
        ("SelectOption", schema_for!(SelectOption)),
        ("RelationshipField", schema_for!(RelationshipField)),
        (
            "RelationshipSelectedItem",
            schema_for!(RelationshipSelectedItem),
        ),
        ("UploadField", schema_for!(UploadField)),
        ("JoinField", schema_for!(JoinField)),
        ("JoinItem", schema_for!(JoinItem)),
        ("GroupField", schema_for!(GroupField)),
        ("RowField", schema_for!(RowField)),
        ("TabsField", schema_for!(TabsField)),
        ("TabPanel", schema_for!(TabPanel)),
        ("ArrayField", schema_for!(ArrayField)),
        ("ArrayRow", schema_for!(ArrayRow)),
        ("BlocksField", schema_for!(BlocksField)),
        ("BlockDefinition", schema_for!(BlockDefinition)),
        ("BlockRow", schema_for!(BlockRow)),
        ("AuthCollection", schema_for!(AuthCollection)),
        ("CollectionEntry", schema_for!(CollectionEntry)),
        ("CollectionCard", schema_for!(CollectionCard)),
        ("GlobalCard", schema_for!(GlobalCard)),
        ("UploadFormContext", schema_for!(UploadFormContext)),
        ("UploadInfo", schema_for!(UploadInfo)),
    ]
}

// ── Schema walker (JSON Schema → FieldDoc list) ────────────────────

fn fields_from_schema(schema: &Value) -> Vec<FieldDoc> {
    let Some(props) = schema.get("properties").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    props
        .iter()
        .map(|(name, prop)| FieldDoc {
            name: name.clone(),
            ty: render_type(prop),
            required: required.contains(&name.as_str()),
            description: strip_rustdoc_links(
                &prop
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .replace('\n', " "),
            ),
        })
        .collect()
}

/// Type-level doc comment from a schema root (empty when absent). Line
/// structure is preserved so Markdown lists in doc comments render; rustdoc
/// cross-links are stripped (their targets mean nothing in mdbook).
fn description_from_schema(schema: &Value) -> String {
    let raw = schema
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    strip_rustdoc_links(raw)
}

/// Convert rustdoc link syntax to plain inline code: `` [`X`](path) `` and
/// `` [`X`] `` both become `` `X` ``. Doc comments are written for rustdoc;
/// their link targets (`crate::…`, `super::…`) are dead in mdbook output.
fn strip_rustdoc_links(s: &str) -> String {
    static LINKED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[(`[^`\]]+`)\]\([A-Za-z0-9_:#.]+\)").expect("valid regex"));
    static BARE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[(`[^`\]]+`)\]([^(]|$)").expect("valid regex"));

    let s = LINKED.replace_all(s, "$1");
    BARE.replace_all(&s, "$1$2").into_owned()
}

/// For a property-less internally-tagged enum schema (e.g. `FieldContext`):
/// render a one-liner naming the tag field and listing every discriminator
/// value. Returns an empty string for plain struct schemas.
fn enum_note_from_schema(schema: &Value) -> String {
    // Tagged enums render as `oneOf`; untagged ones as `anyOf`.
    let Some(variants) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(|v| v.as_array())
    else {
        return String::new();
    };

    // The tag field is the one property every variant pins to a constant.
    // Without one the enum is untagged — render the union of variant types.
    let Some(tag_field) = variants
        .first()
        .and_then(|v| v.get("properties"))
        .and_then(|p| p.as_object())
        .and_then(|props| {
            props
                .iter()
                .find(|(_, p)| p.get("const").is_some())
                .map(|(k, _)| k.clone())
        })
    else {
        let union = variants
            .iter()
            .map(render_type)
            .collect::<Vec<_>>()
            .join(" \\| ");
        return format!("Untagged union — one of: {union}.");
    };

    let tags: Vec<String> = variants
        .iter()
        .filter_map(|v| {
            v.get("properties")?
                .get(&tag_field)?
                .get("const")?
                .as_str()
                .map(|s| format!("`{s}`"))
        })
        .collect();

    if tags.is_empty() {
        return String::new();
    }

    format!(
        "Internally tagged enum — `{tag_field}` selects the variant: {}. \
         Every variant flattens its variant struct (defined below) into the \
         top level, so templates read `{tag_field}` plus the struct's fields \
         directly.",
        tags.join(", ")
    )
}

/// Render a JSON-schema property's type as a Rust-ish string. Refs become
/// markdown links into the definitions section; primitives stay plain.
fn render_type(prop: &Value) -> String {
    if let Some(r) = prop.get("$ref").and_then(|v| v.as_str()) {
        let name = r.rsplit('/').next().unwrap_or(r);
        return format!("[{}](#{})", name, name.to_lowercase());
    }

    if let Some(types) = prop.get("type").and_then(|v| v.as_array()) {
        let non_null: Vec<&str> = types
            .iter()
            .filter_map(|t| t.as_str())
            .filter(|t| *t != "null")
            .collect();
        let nullable = types.iter().any(|t| t.as_str() == Some("null"));

        if non_null.len() == 1 {
            let inner = format_simple_type(non_null[0], prop);
            return if nullable {
                format!("Option&lt;{inner}&gt;")
            } else {
                inner
            };
        }
    }

    if let Some(t) = prop.get("type").and_then(|v| v.as_str()) {
        return format_simple_type(t, prop);
    }

    if let Some(any) = prop.get("anyOf").and_then(|v| v.as_array()) {
        return any
            .iter()
            .map(render_type)
            .collect::<Vec<_>>()
            .join(" \\| ");
    }
    if let Some(one) = prop.get("oneOf").and_then(|v| v.as_array()) {
        return one
            .iter()
            .map(render_type)
            .collect::<Vec<_>>()
            .join(" \\| ");
    }

    "any".to_string()
}

fn format_simple_type(t: &str, prop: &Value) -> String {
    match t {
        "array" => {
            let item_ty = prop
                .get("items")
                .map_or_else(|| "any".to_string(), render_type);
            format!("Vec&lt;{item_ty}&gt;")
        }
        "object" => "Object".to_string(),
        other => other.to_string(),
    }
}

// ── Doc generation ─────────────────────────────────────────────────

fn build_doc_context() -> DocContext {
    let pages: Vec<PageDoc> = pages()
        .map(|p| {
            let schema = (p.schema)();
            PageDoc {
                heading: p.heading,
                page_type: p.page_type,
                template: p.template,
                fields: fields_from_schema(&schema.to_value()),
            }
        })
        .collect();

    let definitions: Vec<TypeDoc> = definitions()
        .into_iter()
        .map(|(name, schema)| {
            let value = schema.to_value();
            TypeDoc {
                name,
                description: description_from_schema(&value),
                enum_note: enum_note_from_schema(&value),
                fields: fields_from_schema(&value),
            }
        })
        .collect();

    DocContext { pages, definitions }
}

/// Render the full `template-context.md` content from the typed
/// page-context structs. Pure function — no I/O. Consumed by the
/// `cargo xtask gen-template-doc` subcommand (write / `--check` diff
/// modes) and by the local-drift assertion test below.
///
/// Public so the xtask binary can call it via the
/// [`crate::docgen`](crate::docgen) re-export — the module itself
/// stays `pub(crate)` because nothing else in the public surface
/// needs to walk these typed schemas at runtime.
///
/// # Panics
///
/// Panics only on programmer error: the inline Handlebars template
/// failing to parse, or the typed `DocContext` failing to render
/// against it. Both are static — neither depends on runtime input —
/// so a panic here means the source has been broken at build time
/// and CI / `cargo test` will fail before any release.
#[must_use]
pub fn generate_template_context_md() -> String {
    let mut hb = Handlebars::new();
    hb.set_strict_mode(true);
    // The output is Markdown, not HTML — turn off HTML-escaping so backticks,
    // quotes, and angle brackets in descriptions render verbatim.
    hb.register_escape_fn(handlebars::no_escape);
    hb.register_template_string("template-context", TEMPLATE)
        .expect("inline template parses");

    let ctx = build_doc_context();
    hb.render("template-context", &ctx)
        .expect("template renders against typed DocContext")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn template_context_doc_is_in_sync() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/src/admin-ui/reference/template-context.md");

        let generated = generate_template_context_md();
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "Could not read {} — regenerate with: cargo xtask gen-template-doc",
                path.display()
            )
        });

        if generated == committed {
            return;
        }

        let g_lines: Vec<&str> = generated.lines().collect();
        let c_lines: Vec<&str> = committed.lines().collect();
        let mut hint = String::new();
        let mut shown = 0usize;
        for (i, (g, c)) in g_lines.iter().zip(c_lines.iter()).enumerate() {
            if g != c {
                let _ = writeln!(
                    hint,
                    "  line {}:\n    committed: {}\n    generated: {}",
                    i + 1,
                    c,
                    g
                );
                shown += 1;
                if shown >= 3 {
                    hint.push_str("  ... (truncated)\n");
                    break;
                }
            }
        }
        if g_lines.len() != c_lines.len() {
            let _ = writeln!(
                hint,
                "  line counts differ: committed={}, generated={}",
                c_lines.len(),
                g_lines.len()
            );
        }

        panic!(
            "template-context.md is out of sync with the typed page contexts.\n\
             Regenerate with:\n\
             \n  cargo xtask gen-template-doc\n\
             \nFirst differing lines:\n{hint}"
        );
    }
}
