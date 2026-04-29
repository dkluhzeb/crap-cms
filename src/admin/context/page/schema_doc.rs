//! Auto-generated reference doc for the typed admin page contexts.
//!
//! Walks every per-page typed context (login, dashboard, collection edit,
//! …), produces a Markdown reference at
//! `docs/src/admin-ui/template-context.md`, and verifies (via the
//! [`template_context_doc_is_in_sync`] test) that the committed file matches
//! what the typed structs would produce. Run with
//! `UPDATE_SCHEMA_DOC=1 cargo test template_context_doc_is_in_sync` to bless
//! changes after touching a page-context struct.
//!
//! The Markdown structure lives in an inline Handlebars template (we already
//! depend on the engine for admin pages, so reusing it keeps the doc layout
//! editable as a template and out of Rust string concat). The Rust side
//! preprocesses each per-page schema into a flat `DocContext` struct that
//! the template renders.

use handlebars::Handlebars;
use schemars::{Schema, schema_for};
use serde::Serialize;
use serde_json::Value;

use crate::admin::context::page::{
    auth::{ForgotPasswordPage, LoginPage, MfaPage, ResetPasswordPage},
    collections::{
        CollectionCreatePage, CollectionDeleteConfirmPage, CollectionEditPage,
        CollectionFormErrorPage, CollectionItemsListPage, CollectionListPage,
        CollectionRestoreConfirmPage, CollectionVersionsListPage,
    },
    dashboard::DashboardPage,
    errors::ErrorPage,
    globals::{
        GlobalEditPage, GlobalFormErrorPage, GlobalRestoreConfirmPage, GlobalVersionsListPage,
    },
};

// ── Handlebars template ─────────────────────────────────────────────

/// Inline Handlebars template that produces the Markdown doc. Whitespace
/// outside `{{ }}` blocks is significant — keep the structure tight so the
/// rendered output stays clean.
const TEMPLATE: &str = r#"<!--
  AUTO-GENERATED — do not edit by hand.
  Source of truth: typed page-context structs in `src/admin/context/page/`.
  Regenerate with: `UPDATE_SCHEMA_DOC=1 cargo test template_context_doc_is_in_sync`
-->

# Admin template context reference

Every admin page renders a typed Rust struct serialized to JSON, runs it through the optional `before_render` Lua hook, and hands it to Handlebars. This file lists every page, its `page.type` discriminant, the template it renders, and the fields the template can rely on.

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

{{#if fields}}
{{#each fields}}
- **`{{name}}`** ({{{ty}}}){{#unless required}} _(optional)_{{/unless}}{{#if description}} — {{description}}{{/if}}
{{/each}}
{{else}}
_(No fields.)_
{{/if}}

{{/each}}
"#;

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

fn pages() -> Vec<PageEntry> {
    vec![
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
        PageEntry {
            heading: "Error pages (403 / 404 / 500)",
            page_type: "error_403 | error_404 | error_500",
            template: "errors/{403,404,500}",
            schema: || schema_for!(ErrorPage),
        },
        PageEntry {
            heading: "Dashboard",
            page_type: "dashboard",
            template: "dashboard/index",
            schema: || schema_for!(DashboardPage),
        },
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
    ]
}

fn definitions() -> Vec<(&'static str, Schema)> {
    use crate::admin::context::{
        AuthBasePageContext, BasePageContext, Breadcrumb, CollectionContext, CrapMeta, DocumentRef,
        EditorLocaleOption, FieldContext, GlobalContext, LocaleTemplateOption, NavCollection,
        NavData, NavGlobal, PageMeta, PaginationContext, UserContext,
        page::auth::AuthCollection,
        page::collections::{CollectionEntry, UploadFormContext, UploadInfo},
        page::dashboard::{CollectionCard, GlobalCard},
    };

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
        ("GlobalContext", schema_for!(GlobalContext)),
        ("DocumentRef", schema_for!(DocumentRef)),
        ("PaginationContext", schema_for!(PaginationContext)),
        ("FieldContext", schema_for!(FieldContext)),
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
            description: prop
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .replace('\n', " "),
        })
        .collect()
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
                format!("Option<{}>", inner)
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
                .map(render_type)
                .unwrap_or_else(|| "any".to_string());
            format!("Vec<{}>", item_ty)
        }
        "object" => "Object".to_string(),
        other => other.to_string(),
    }
}

// ── Doc generation ─────────────────────────────────────────────────

fn build_doc_context() -> DocContext {
    let pages: Vec<PageDoc> = pages()
        .into_iter()
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
        .map(|(name, schema)| TypeDoc {
            name,
            fields: fields_from_schema(&schema.to_value()),
        })
        .collect();

    DocContext { pages, definitions }
}

fn generate_template_context_md() -> String {
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

#[test]
fn template_context_doc_is_in_sync() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/src/admin-ui/template-context.md");

    let generated = generate_template_context_md();

    if std::env::var("UPDATE_SCHEMA_DOC").is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create docs parent");
        }
        std::fs::write(&path, &generated).expect("write template-context.md");
        eprintln!("Wrote {} bytes to {}", generated.len(), path.display());
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "Could not read {} — bless it with: \
             UPDATE_SCHEMA_DOC=1 cargo test template_context_doc_is_in_sync",
            path.display()
        )
    });

    if generated != committed {
        let g_lines: Vec<&str> = generated.lines().collect();
        let c_lines: Vec<&str> = committed.lines().collect();
        let mut hint = String::new();
        let mut shown = 0usize;
        for (i, (g, c)) in g_lines.iter().zip(c_lines.iter()).enumerate() {
            if g != c {
                hint.push_str(&format!(
                    "  line {}:\n    committed: {}\n    generated: {}\n",
                    i + 1,
                    c,
                    g
                ));
                shown += 1;
                if shown >= 3 {
                    hint.push_str("  ... (truncated)\n");
                    break;
                }
            }
        }
        if g_lines.len() != c_lines.len() {
            hint.push_str(&format!(
                "  line counts differ: committed={}, generated={}\n",
                c_lines.len(),
                g_lines.len()
            ));
        }

        panic!(
            "template-context.md is out of sync with the typed page contexts.\n\
             Regenerate with:\n\
             \n  UPDATE_SCHEMA_DOC=1 cargo test template_context_doc_is_in_sync\n\
             \nFirst differing lines:\n{}",
            hint
        );
    }
}
