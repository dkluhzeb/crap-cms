//! Data-aware in-place field-read/write strip using an already-held `&Lua`.
//! The VM-integrated wrappers over the pure strip walker in [`super::walk`].

use mlua::Lua;
use serde_json::{Map, Value};

use tracing::warn;

use super::walk::{
    extract_read_access, has_any_field_access, strip_access_data_aware,
    strip_read_access_data_aware,
};
use crate::core::{Document, DocumentFields, FieldDefinition, HookRef};
use crate::db::AccessResult;
use crate::hooks::lifecycle::{AccessCheckInput, access::collection::check_access_with_lua};

/// Data-aware field-**read** strip using an already-held `&Lua`. Walks `level`
/// (the universal `serde_json::Map` form shared by read documents, version
/// snapshots, populated targets, and live-event payloads) and removes every
/// read-denied field in place. Each field's `access.read` function is evaluated
/// with `ctx.data` = the field's own immediate level (the row, for a field
/// inside an array/blocks row) and `ctx.document` = the full `document` (stable
/// as the walk descends) — harmonized with `FieldHookContext`. Mirrors
/// [`check_field_read_access_with_lua`] but data-aware and in place.
///
/// Context for a data-aware field-**read** strip: the full document
/// (`ctx.document`), its collection slug (`ctx.collection`), the requester, and
/// the target locale. The read-path mirror of [`WriteStripInput`]; grouped so the
/// strip entry points stay within a sane argument count.
pub struct ReadStripInput<'a> {
    pub document: &'a DocumentFields,
    pub collection: &'a str,
    pub user: Option<&'a Document>,
    pub locale: Option<&'a str>,
}

/// Gated on [`has_any_field_access`]: when no field carries an `access.read`
/// function the call returns immediately with zero per-document work, so the
/// data-aware path costs nothing on schemas that don't use field read access.
pub(crate) fn strip_read_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    input: &ReadStripInput<'_>,
) {
    if !has_any_field_access(fields, extract_read_access) {
        return;
    }

    let is_denied = |hook: &HookRef, data: &DocumentFields| -> bool {
        match check_access_with_lua(
            lua,
            &AccessCheckInput::builder("read", input.collection)
                .access(Some(hook))
                .user(input.user)
                .data(Some(data))
                .document(Some(input.document))
                .locale(input.locale)
                .build(),
        ) {
            Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => false,
            Ok(AccessResult::Denied) => true,
            Err(e) => {
                warn!(
                    "Field read access function '{}' error (treating as denied): {}",
                    hook.reference(),
                    e
                );

                true
            }
        }
    };

    strip_read_access_data_aware(fields, level, &is_denied);
}

/// Context for a data-aware field-**write** strip: the full incoming document
/// (`ctx.document`), the requester, the target locale, and the write operation.
/// Grouped into a struct so the strip entry points stay within a sane argument
/// count and read/write callers thread one value.
pub struct WriteStripInput<'a> {
    pub document: &'a DocumentFields,
    /// The collection (or global) slug, exposed to field-access rules as
    /// `ctx.collection`.
    pub collection: &'a str,
    pub user: Option<&'a Document>,
    pub locale: Option<&'a str>,
    /// `"create"` or `"update"`; any other value makes the strip a no-op.
    pub operation: &'a str,
}

/// Data-aware field-**write** strip (create/update) using an already-held
/// `&Lua`. Removes from `level` every field the user may not write for
/// `input.operation`, evaluating each field's `access.create` / `access.update`
/// rule with `ctx.data` = the field's own immediate level and `ctx.document` =
/// the full incoming `input.document`. The write-path mirror of
/// [`strip_read_access_with_lua`], so a field-access rule reading `ctx.data` /
/// `ctx.document` behaves identically on read and write.
///
/// Gated on [`has_any_field_access`]: zero per-document work when no field
/// configures the relevant write-access function.
pub(crate) fn strip_write_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    input: &WriteStripInput<'_>,
) {
    let extract: fn(&FieldDefinition) -> Option<&HookRef> = match input.operation {
        "create" => |f| f.access.create.as_ref(),
        "update" => |f| f.access.update.as_ref(),
        _ => return,
    };

    if !has_any_field_access(fields, extract) {
        return;
    }

    let is_denied = |hook: &HookRef, data: &DocumentFields| -> bool {
        match check_access_with_lua(
            lua,
            &AccessCheckInput::builder(input.operation, input.collection)
                .access(Some(hook))
                .user(input.user)
                .data(Some(data))
                .document(Some(input.document))
                .locale(input.locale)
                .build(),
        ) {
            Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => false,
            Ok(AccessResult::Denied) => true,
            Err(e) => {
                warn!(
                    "Field {} access function '{}' error (treating as denied): {}",
                    input.operation,
                    hook.reference(),
                    e
                );

                true
            }
        }
    };

    strip_access_data_aware(fields, level, &extract, &is_denied);
}

#[cfg(test)]
mod tests {
    use super::super::super::test_helpers::*;
    use super::*;
    use crate::core::{FieldAccess, FieldDefinition, FieldType};
    use serde_json::json;

    // ── Data-aware strip via a real Lua VM (ctx.data / ctx.document) ─────────

    fn read_by(name: &str, func: &str) -> FieldDefinition {
        make_field(
            name,
            FieldAccess {
                read: Some(func.into()),
                ..Default::default()
            },
        )
    }

    /// A real Lua rule keyed on the field's OWN array-row level (`ctx.data`)
    /// keeps `premium` in `kind == "public"` rows and strips it from others.
    #[test]
    fn strip_read_access_with_lua_uses_per_row_sibling_data() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("kind", FieldType::Text).build(),
                    read_by("premium", "test_access.allow_if_kind_public"),
                ])
                .build(),
        ];

        let mut doc = json!({
            "items": [
                { "kind": "public", "premium": "a" },
                { "kind": "private", "premium": "b" }
            ]
        })
        .as_object()
        .unwrap()
        .clone();
        let document: DocumentFields = doc.clone().into_iter().collect();

        strip_read_access_with_lua(
            &lua,
            &fields,
            &mut doc,
            &ReadStripInput {
                document: &document,
                collection: "",
                user: None,
                locale: None,
            },
        );

        let rows = doc.get("items").unwrap().as_array().unwrap();
        assert_eq!(
            rows[0].as_object().unwrap().get("premium").unwrap(),
            &json!("a"),
            "public row keeps premium"
        );
        assert!(
            !rows[1].as_object().unwrap().contains_key("premium"),
            "private row strips premium"
        );
    }

    /// A real Lua rule keyed on the FULL document (`ctx.document`) keeps the
    /// field when the document is published and strips it when it is a draft.
    #[test]
    fn strip_read_access_with_lua_uses_full_document() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("status", FieldType::Text).build(),
            read_by("secret", "test_access.allow_if_doc_published"),
        ];

        let mut published = json!({ "status": "published", "secret": "x" })
            .as_object()
            .unwrap()
            .clone();
        let pd: DocumentFields = published.clone().into_iter().collect();
        strip_read_access_with_lua(
            &lua,
            &fields,
            &mut published,
            &ReadStripInput {
                document: &pd,
                collection: "",
                user: None,
                locale: None,
            },
        );
        assert!(published.contains_key("secret"), "published → secret kept");

        let mut draft = json!({ "status": "draft", "secret": "x" })
            .as_object()
            .unwrap()
            .clone();
        let dd: DocumentFields = draft.clone().into_iter().collect();
        strip_read_access_with_lua(
            &lua,
            &fields,
            &mut draft,
            &ReadStripInput {
                document: &dd,
                collection: "",
                user: None,
                locale: None,
            },
        );
        assert!(!draft.contains_key("secret"), "draft → secret stripped");
    }

    /// `ctx.document` stays the full document as the walk descends into array
    /// rows: a rule reading `ctx.document.status` strips a per-row field from
    /// EVERY row of a draft document, even though the rows carry no `status`.
    #[test]
    fn strip_read_access_with_lua_document_is_stable_inside_rows() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("status", FieldType::Text).build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![read_by("note", "test_access.allow_if_doc_published")])
                .build(),
        ];

        let mut doc = json!({
            "status": "draft",
            "items": [ { "note": "a" }, { "note": "b" } ]
        })
        .as_object()
        .unwrap()
        .clone();
        let document: DocumentFields = doc.clone().into_iter().collect();

        strip_read_access_with_lua(
            &lua,
            &fields,
            &mut doc,
            &ReadStripInput {
                document: &document,
                collection: "",
                user: None,
                locale: None,
            },
        );

        let rows = doc.get("items").unwrap().as_array().unwrap();
        assert!(
            !rows[0].as_object().unwrap().contains_key("note"),
            "draft document strips note from row 0 (ctx.document.status seen inside the row)"
        );
        assert!(
            !rows[1].as_object().unwrap().contains_key("note"),
            "draft document strips note from row 1"
        );
    }

    /// Write-path mirror: a per-row `access.create` rule strips `premium` from
    /// rows whose `kind` isn't "public", proving `ctx.data` is the write level.
    #[test]
    fn strip_write_access_with_lua_strips_create_denied_per_row() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("kind", FieldType::Text).build(),
                    make_field(
                        "premium",
                        FieldAccess {
                            create: Some("test_access.allow_if_kind_public".into()),
                            ..Default::default()
                        },
                    ),
                ])
                .build(),
        ];

        let mut doc = json!({
            "items": [
                { "kind": "public", "premium": "a" },
                { "kind": "private", "premium": "b" }
            ]
        })
        .as_object()
        .unwrap()
        .clone();
        let document: DocumentFields = doc.clone().into_iter().collect();

        strip_write_access_with_lua(
            &lua,
            &fields,
            &mut doc,
            &WriteStripInput {
                document: &document,
                collection: "",
                user: None,
                locale: None,
                operation: "create",
            },
        );

        let rows = doc.get("items").unwrap().as_array().unwrap();
        assert_eq!(
            rows[0].as_object().unwrap().get("premium").unwrap(),
            &json!("a"),
            "public row keeps create-gated premium"
        );
        assert!(
            !rows[1].as_object().unwrap().contains_key("premium"),
            "private row strips create-gated premium"
        );
    }

    /// An operation other than create/update strips nothing (no extractor).
    #[test]
    fn strip_write_access_with_lua_unknown_operation_is_noop() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "x",
            FieldAccess {
                update: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];

        let mut doc = json!({ "x": 1 }).as_object().unwrap().clone();
        let document: DocumentFields = doc.clone().into_iter().collect();

        strip_write_access_with_lua(
            &lua,
            &fields,
            &mut doc,
            &WriteStripInput {
                document: &document,
                collection: "",
                user: None,
                locale: None,
                operation: "delete",
            },
        );

        assert!(doc.contains_key("x"), "unknown operation strips nothing");
    }

    // ── Nested group strip (canonical shape) ─────────────────────────────────

    fn group_with(name: &str, sub: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Group)
            .fields(sub)
            .build()
    }

    /// A create-denied sub-field inside a nested group object is stripped from
    /// that object; the allowed sibling stays.
    #[test]
    fn strip_write_access_strips_nested_group_subfield() {
        let lua = setup_lua();
        let fields = vec![group_with(
            "seo",
            vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
                make_field(
                    "secret",
                    FieldAccess {
                        create: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                ),
            ],
        )];

        let mut doc = json!({ "seo": { "title": "t", "secret": "x" } })
            .as_object()
            .unwrap()
            .clone();
        let document: DocumentFields = doc.clone().into_iter().collect();

        strip_write_access_with_lua(
            &lua,
            &fields,
            &mut doc,
            &WriteStripInput {
                document: &document,
                collection: "",
                user: None,
                locale: None,
                operation: "create",
            },
        );

        let seo = doc.get("seo").unwrap().as_object().unwrap();
        assert_eq!(
            seo.get("title"),
            Some(&json!("t")),
            "allowed sub-field kept"
        );
        assert!(
            !seo.contains_key("secret"),
            "create-denied nested group sub-field must be stripped"
        );
    }

    /// A read-denied sub-field inside a nested group object is stripped.
    #[test]
    fn strip_read_access_strips_nested_group_subfield() {
        let lua = setup_lua();
        let fields = vec![group_with(
            "seo",
            vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
                read_by("secret", "test_access.deny"),
            ],
        )];

        let mut doc = json!({ "seo": { "title": "t", "secret": "x" } })
            .as_object()
            .unwrap()
            .clone();
        let document: DocumentFields = doc.clone().into_iter().collect();

        strip_read_access_with_lua(
            &lua,
            &fields,
            &mut doc,
            &ReadStripInput {
                document: &document,
                collection: "",
                user: None,
                locale: None,
            },
        );

        let seo = doc.get("seo").unwrap().as_object().unwrap();
        assert!(seo.contains_key("title"));
        assert!(
            !seo.contains_key("secret"),
            "read-denied nested group sub-field must be stripped"
        );
    }

    /// A denied whole group (access on the group field itself) removes the whole
    /// nested group object; sibling top-level fields are untouched.
    #[test]
    fn strip_read_access_removes_whole_group_when_denied() {
        let lua = setup_lua();
        let mut seo = group_with(
            "seo",
            vec![FieldDefinition::builder("title", FieldType::Text).build()],
        );
        seo.access.read = Some("test_access.deny".into());
        let fields = vec![seo];

        let mut doc = json!({ "seo": { "title": "t" }, "keep": "y" })
            .as_object()
            .unwrap()
            .clone();
        let document: DocumentFields = doc.clone().into_iter().collect();

        strip_read_access_with_lua(
            &lua,
            &fields,
            &mut doc,
            &ReadStripInput {
                document: &document,
                collection: "",
                user: None,
                locale: None,
            },
        );

        assert!(
            !doc.contains_key("seo"),
            "whole denied group object must be stripped"
        );
        assert!(doc.contains_key("keep"), "sibling field untouched");
    }
}
