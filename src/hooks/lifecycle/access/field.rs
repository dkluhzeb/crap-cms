//! Field-level read/write access checks plus the recursive helpers that build
//! the per-field denied list — flat columns/groups and fields nested inside
//! array/blocks rows at any depth. `WriteHooks::check_access` and the read
//! post-processing apply the resulting [`FieldDenial`]s to strip denied fields.

use mlua::Lua;
use serde_json::{Map, Value};
use tracing::warn;

use crate::{
    core::{
        DenialSeg, Document, DocumentFields, FieldDefinition, FieldDenial, FieldType, HookRef,
        any_field,
    },
    db::{AccessResult, query::helpers::prefixed_name},
    hooks::lifecycle::{AccessCheckInput, access::collection::check_access_with_lua},
};

pub(crate) fn check_field_read_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
) -> Vec<FieldDenial> {
    collect_field_access_denied(
        lua,
        fields,
        collection,
        user,
        locale,
        extract_read_access,
        "read",
    )
}

/// Check field-level write access using an already-held `&Lua` reference.
/// Returns the fields to strip from the input. Recurses into Group (with `__`
/// prefix), transparent layout containers, and array/blocks rows.
pub(crate) fn check_field_write_access_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> Vec<FieldDenial> {
    let extractor: fn(&FieldDefinition) -> Option<&HookRef> = match operation {
        "create" => extract_create_access,
        "update" => extract_update_access,
        _ => return Vec::new(),
    };

    collect_field_access_denied(lua, fields, collection, user, locale, extractor, operation)
}

fn extract_read_access(f: &FieldDefinition) -> Option<&HookRef> {
    f.access.read.as_ref()
}

fn extract_create_access(f: &FieldDefinition) -> Option<&HookRef> {
    f.access.create.as_ref()
}

fn extract_update_access(f: &FieldDefinition) -> Option<&HookRef> {
    f.access.update.as_ref()
}

/// Evaluate one field's access function. `true` = denied (or errored, which is
/// fail-closed to denied).
fn access_denied(
    lua: &Lua,
    hook: &HookRef,
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> bool {
    match check_access_with_lua(
        lua,
        &AccessCheckInput {
            access: Some(hook),
            user,
            id: None,
            data: None,
            // Threaded by the data-aware field-strip walker; `None` here on the
            // legacy document-independent path.
            document: None,
            locale,
            operation,
            collection,
            ui_locale: None,
        },
    ) {
        Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => false,
        Ok(AccessResult::Denied) => true,
        Err(e) => {
            warn!(
                "Field access function '{}' error (treating as denied): {}",
                hook.reference(),
                e
            );

            true
        }
    }
}

/// Collect denials by evaluating each field's access function via Lua.
fn collect_field_access_denied(
    lua: &Lua,
    fields: &[FieldDefinition],
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    extractor: fn(&FieldDefinition) -> Option<&HookRef>,
    operation: &str,
) -> Vec<FieldDenial> {
    let is_denied = |field: &FieldDefinition| {
        extractor(field)
            .is_some_and(|hook| access_denied(lua, hook, collection, user, locale, operation))
    };

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

/// Check whether any field — at any depth, including inside array/blocks rows —
/// has an access function for the given extractor.
pub(crate) fn has_any_field_access(
    fields: &[FieldDefinition],
    extractor: fn(&FieldDefinition) -> Option<&HookRef>,
) -> bool {
    any_field(fields, &|field| extractor(field).is_some())
}

/// Shared denial-tree walker at the document level (flat columns).
///
/// Parameterized by `is_denied` so the Lua-evaluated path and the fail-closed
/// deny-all path (`runner::access`) share the identical container-recursion
/// rules — there is exactly one place that encodes how access descends the
/// field tree.
///
/// - Group recurses with a `parent__` column prefix.
/// - Row/Collapsible/Tabs are transparent (same prefix).
/// - Array/Blocks switch to [`collect_denials_nested`]: their data is JSON, so
///   denials become per-row paths keyed by the array's data key.
pub(crate) fn collect_denials_flat<F: Fn(&FieldDefinition) -> bool>(
    fields: &[FieldDefinition],
    is_denied: &F,
    prefix: &str,
    out: &mut Vec<FieldDenial>,
) {
    for field in fields {
        let full_name = prefixed_name(prefix, &field.name);

        if is_denied(field) {
            out.push(FieldDenial::Flat(full_name));

            continue; // Parent denied → its sub-fields go with it.
        }

        match field.field_type {
            FieldType::Group => collect_denials_flat(&field.fields, is_denied, &full_name, out),
            FieldType::Row | FieldType::Collapsible => {
                collect_denials_flat(&field.fields, is_denied, prefix, out);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_denials_flat(&tab.fields, is_denied, prefix, out);
                }
            }
            FieldType::Array => {
                collect_denials_nested(&field.fields, is_denied, &full_name, None, &[], out);
            }
            FieldType::Blocks => {
                for block in &field.blocks {
                    collect_denials_nested(
                        &block.fields,
                        is_denied,
                        &full_name,
                        Some(&block.block_type),
                        &[],
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Shared denial-tree walker inside array/blocks rows. Emits [`FieldDenial::Nested`]
/// keyed by the top-level `array_key` and the within-row `row_path` (groups and
/// nested arrays), so the strip can reach every row at any depth.
pub(crate) fn collect_denials_nested<F: Fn(&FieldDefinition) -> bool>(
    fields: &[FieldDefinition],
    is_denied: &F,
    array_key: &str,
    array_block_type: Option<&str>,
    row_path: &[DenialSeg],
    out: &mut Vec<FieldDenial>,
) {
    for field in fields {
        if is_denied(field) {
            out.push(FieldDenial::Nested {
                array_key: array_key.to_string(),
                array_block_type: array_block_type.map(str::to_string),
                row_path: row_path.to_vec(),
                leaf: field.name.clone(),
            });

            continue;
        }

        match field.field_type {
            FieldType::Group => {
                let mut path = row_path.to_vec();
                path.push(DenialSeg::Group(field.name.clone()));
                collect_denials_nested(
                    &field.fields,
                    is_denied,
                    array_key,
                    array_block_type,
                    &path,
                    out,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_denials_nested(
                    &field.fields,
                    is_denied,
                    array_key,
                    array_block_type,
                    row_path,
                    out,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_denials_nested(
                        &tab.fields,
                        is_denied,
                        array_key,
                        array_block_type,
                        row_path,
                        out,
                    );
                }
            }
            FieldType::Array => {
                let mut path = row_path.to_vec();
                path.push(DenialSeg::Rows {
                    key: field.name.clone(),
                    block_type: None,
                });
                collect_denials_nested(
                    &field.fields,
                    is_denied,
                    array_key,
                    array_block_type,
                    &path,
                    out,
                );
            }
            FieldType::Blocks => {
                // One `Rows` step per block type so the strip only touches rows
                // of that type — a denial in one block must not strip a
                // same-named field from a sibling block type.
                for block in &field.blocks {
                    let mut path = row_path.to_vec();
                    path.push(DenialSeg::Rows {
                        key: field.name.clone(),
                        block_type: Some(block.block_type.clone()),
                    });
                    collect_denials_nested(
                        &block.fields,
                        is_denied,
                        array_key,
                        array_block_type,
                        &path,
                        out,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Data-aware in-place field-**read** strip. Walks `level` (a document object)
/// and, for every read-gated field, calls `is_denied(hook, level_snapshot)` —
/// the snapshot being the field's own level (`ctx.data`); the caller's closure
/// supplies `ctx.document`. A denied field/group/array/blocks field is removed
/// in place; for arrays/blocks the rule is evaluated **per row** (each row is
/// its own level), so a data-dependent rule can keep some rows' values and drop
/// others'.
///
/// Container recursion mirrors [`super::super::execution`]'s `FieldHookWalker`
/// (the canonical in-row traversal): Group navigates into the nested object,
/// Row/Collapsible/Tabs are transparent (same level), Array/Blocks recurse per
/// row. `level` is the universal nested-object form (`serde_json::Map`) shared
/// by read documents, version snapshots, and populate targets, so a single
/// walker covers every read-strip surface.
///
/// Parameterized by `is_denied` (like [`collect_denials_flat`]) so it is
/// unit-testable without a live VM and so the Lua-evaluation logic lives in one
/// place at the call site.
pub(crate) fn strip_read_access_data_aware<F: Fn(&HookRef, &DocumentFields) -> bool>(
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    is_denied: &F,
) {
    strip_access_data_aware(fields, level, &|f| f.access.read.as_ref(), is_denied);
}

/// Generic data-aware in-place field strip shared by the read- and write-access
/// paths. `extract` selects which access function gates each field
/// (`access.read` for reads, `access.create` / `access.update` for writes); the
/// recursion and `ctx.data` (per-level) wiring is identical for both.
///
/// Operates on the canonical **nested** document shape (group data is a nested
/// object at every level), structurally identical to the [`FieldHookWalker`]:
/// Group navigates into its object, Row/Collapsible/Tabs are transparent,
/// Array/Blocks recurse per row. The whole write/read pipeline canonicalizes to
/// nested before access runs (ingress `nest_group_fields`, reads hydrate), so
/// there is no flat `group__sub` form to handle here.
///
/// [`FieldHookWalker`]: crate::hooks::lifecycle::execution
pub(crate) fn strip_access_data_aware<E, F>(
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    extract: &E,
    is_denied: &F,
) where
    E: Fn(&FieldDefinition) -> Option<&HookRef>,
    F: Fn(&HookRef, &DocumentFields) -> bool,
{
    // Snapshot the current level for `ctx.data` (read-only sibling view); the
    // real map is mutated (denied fields removed) as we go.
    let level_snapshot: DocumentFields =
        level.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    for field in fields {
        if let Some(hook) = extract(field)
            && is_denied(hook, &level_snapshot)
        {
            level.remove(&field.name);
            continue; // Parent denied → its sub-fields go with it.
        }

        match field.field_type {
            FieldType::Group => {
                if let Some(Value::Object(sub)) = level.get_mut(&field.name) {
                    strip_access_data_aware(&field.fields, sub, extract, is_denied);
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                strip_access_data_aware(&field.fields, level, extract, is_denied);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    strip_access_data_aware(&tab.fields, level, extract, is_denied);
                }
            }
            FieldType::Array => {
                if let Some(Value::Array(rows)) = level.get_mut(&field.name) {
                    for row in rows.iter_mut() {
                        if let Value::Object(r) = row {
                            strip_access_data_aware(&field.fields, r, extract, is_denied);
                        }
                    }
                }
            }
            FieldType::Blocks => {
                if let Some(Value::Array(rows)) = level.get_mut(&field.name) {
                    for row in rows.iter_mut() {
                        let Value::Object(r) = row else { continue };
                        let block_type = r
                            .get("_block_type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if let Some(block) =
                            field.blocks.iter().find(|b| b.block_type == block_type)
                        {
                            strip_access_data_aware(&block.fields, r, extract, is_denied);
                            continue;
                        }

                        // Unresolvable block type (missing/unknown `_block_type`):
                        // we can't map the row's keys to any block's field-access
                        // rules, so a selective strip is impossible. Fail closed —
                        // drop every non-system key rather than pass the row
                        // through unstripped. Stored rows always carry a valid
                        // type, and a forged unknown-type row is rejected by
                        // validation anyway, so this only ever bites malformed
                        // input on the write path.
                        r.retain(|k, _| k.starts_with('_'));
                    }
                }
            }
            _ => {}
        }
    }
}

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
            &AccessCheckInput {
                access: Some(hook),
                user: input.user,
                id: None,
                data: Some(data),
                document: Some(input.document),
                locale: input.locale,
                operation: "read",
                collection: input.collection,
                ui_locale: None,
            },
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
            &AccessCheckInput {
                access: Some(hook),
                user: input.user,
                id: None,
                data: Some(data),
                document: Some(input.document),
                locale: input.locale,
                operation: input.operation,
                collection: input.collection,
                ui_locale: None,
            },
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

/// Data-aware collection of read-denied field **paths** for a single `document`,
/// for surfaces that need denial NAMES (the admin form's input dropping, the
/// `crap.access.field_read_denied` introspection API) rather than stripping
/// values in place. Each `access.read` rule is evaluated with `ctx.data` =
/// `ctx.document` = `document` (document-level context; per-row granularity is
/// only meaningful for the in-place value strip, not for a flat name list).
/// Shares the canonical [`collect_denials_flat`] recursion so the reported paths
/// match what [`strip_read_access_with_lua`] removes.
pub(crate) fn collect_read_denied_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    document: &DocumentFields,
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
) -> Vec<FieldDenial> {
    let is_denied = |field: &FieldDefinition| {
        field.access.read.as_ref().is_some_and(|hook| {
            match check_access_with_lua(
                lua,
                &AccessCheckInput {
                    access: Some(hook),
                    user,
                    id: None,
                    data: Some(document),
                    document: Some(document),
                    locale,
                    operation: "read",
                    collection,
                    ui_locale: None,
                },
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
        })
    };

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

/// Data-aware collection of write-denied field **paths** for a single `document`
/// under `operation` (`"create"` / `"update"`) — the write-side mirror of
/// [`collect_read_denied_with_lua`], for the `crap.access.field_write_denied`
/// introspection API. Each `access.create` / `access.update` rule is evaluated
/// with `ctx.data` = `ctx.document` = `document` (document-level context), so the
/// reported names match what [`strip_write_access_with_lua`] would remove for
/// that document. An unknown `operation` yields no denials.
pub(crate) fn collect_write_denied_with_lua(
    lua: &Lua,
    fields: &[FieldDefinition],
    document: &DocumentFields,
    collection: &str,
    user: Option<&Document>,
    locale: Option<&str>,
    operation: &str,
) -> Vec<FieldDenial> {
    let extract: fn(&FieldDefinition) -> Option<&HookRef> = match operation {
        "create" => extract_create_access,
        "update" => extract_update_access,
        _ => return Vec::new(),
    };

    let is_denied = |field: &FieldDefinition| {
        extract(field).is_some_and(|hook| {
            match check_access_with_lua(
                lua,
                &AccessCheckInput {
                    access: Some(hook),
                    user,
                    id: None,
                    data: Some(document),
                    document: Some(document),
                    locale,
                    operation,
                    collection,
                    ui_locale: None,
                },
            ) {
                Ok(AccessResult::Allowed | AccessResult::Constrained(_)) => false,
                Ok(AccessResult::Denied) => true,
                Err(e) => {
                    warn!(
                        "Field {operation} access function '{}' error (treating as denied): {}",
                        hook.reference(),
                        e
                    );

                    true
                }
            }
        })
    };

    let mut denied = Vec::new();
    collect_denials_flat(fields, &is_denied, "", &mut denied);

    denied
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::*;
    use super::*;
    use crate::core::{FieldAccess, FieldTab, FieldType};

    fn flat(name: &str) -> FieldDenial {
        FieldDenial::Flat(name.to_string())
    }

    // ── Data-aware strip walker (VM-free; mock `is_denied`) ──────────────────

    use crate::core::FieldDefinition;
    use serde_json::json;

    fn read_gated(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(FieldAccess {
                read: Some("h".into()),
                ..Default::default()
            })
            .build()
    }

    /// A doc-dependent rule (hide `secret` unless `status == "published"`)
    /// strips at the document level based on `ctx.data`.
    #[test]
    fn data_aware_strip_top_level_uses_sibling_data() {
        let fields = vec![
            FieldDefinition::builder("status", FieldType::Text).build(),
            read_gated("secret"),
        ];
        // deny `secret` when the level's `status` is not "published"
        let is_denied =
            |_hook: &HookRef, level: &DocumentFields| level.get_str("status") != Some("published");

        let mut published = json!({ "status": "published", "secret": "x" })
            .as_object()
            .unwrap()
            .clone();
        strip_read_access_data_aware(&fields, &mut published, &is_denied);
        assert!(published.contains_key("secret"), "published → secret kept");

        let mut draft = json!({ "status": "draft", "secret": "x" })
            .as_object()
            .unwrap()
            .clone();
        strip_read_access_data_aware(&fields, &mut draft, &is_denied);
        assert!(!draft.contains_key("secret"), "draft → secret stripped");
    }

    /// THE per-row proof: a rule keyed on the row's own data strips the field
    /// from some rows and keeps it in others — `ctx.data` is each row's level.
    #[test]
    fn data_aware_strip_is_per_array_row() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("kind", FieldType::Text).build(),
                    read_gated("premium"),
                ])
                .build(),
        ];
        // deny `premium` only in rows whose `kind` is "free"
        let is_denied =
            |_hook: &HookRef, level: &DocumentFields| level.get_str("kind") == Some("free");

        let mut doc = json!({
            "items": [
                { "kind": "free", "premium": "a" },
                { "kind": "paid", "premium": "b" }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        strip_read_access_data_aware(&fields, &mut doc, &is_denied);

        let rows = doc.get("items").unwrap().as_array().unwrap();
        assert!(
            !rows[0].as_object().unwrap().contains_key("premium"),
            "free row → premium stripped"
        );
        assert_eq!(
            rows[1].as_object().unwrap().get("premium").unwrap(),
            &json!("b"),
            "paid row → premium kept"
        );
    }

    /// A denied group is removed whole; nested group fields are reachable.
    #[test]
    fn data_aware_strip_descends_into_groups() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("public", FieldType::Text).build(),
                    read_gated("token"),
                ])
                .build(),
        ];
        // always deny `token`
        let is_denied = |_hook: &HookRef, _level: &DocumentFields| true;

        let mut doc = json!({ "meta": { "public": "ok", "token": "secret" } })
            .as_object()
            .unwrap()
            .clone();
        strip_read_access_data_aware(&fields, &mut doc, &is_denied);

        let meta = doc.get("meta").unwrap().as_object().unwrap();
        assert!(meta.contains_key("public"), "non-gated group field kept");
        assert!(!meta.contains_key("token"), "gated group field stripped");
    }

    /// Fail-closed: a Blocks row whose `_block_type` resolves to no block
    /// definition (forged/missing type) must NOT pass its data through
    /// unstripped. The walker can't map the row's keys to any field-access rule,
    /// so it drops every non-system key, keeping only `_`-prefixed metadata.
    #[test]
    fn data_aware_strip_drops_unresolved_block_row_data() {
        let fields = vec![
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![crate::core::BlockDefinition::new(
                    "text",
                    vec![FieldDefinition::builder("value", FieldType::Text).build()],
                )])
                .build(),
        ];
        // Never consulted for the unknown row — there is no field to gate.
        let is_denied = |_hook: &HookRef, _level: &DocumentFields| false;

        let mut doc = json!({
            "body": [
                { "_block_type": "text", "value": "kept" },
                { "_block_type": "forged", "smuggled": "leak", "_keepme": "meta" }
            ]
        })
        .as_object()
        .unwrap()
        .clone();

        strip_read_access_data_aware(&fields, &mut doc, &is_denied);

        let rows = doc.get("body").unwrap().as_array().unwrap();
        assert_eq!(
            rows[0].as_object().unwrap().get("value").unwrap(),
            &json!("kept"),
            "known block row passes through normally"
        );
        let forged = rows[1].as_object().unwrap();
        assert!(
            !forged.contains_key("smuggled"),
            "unresolved-block-type row must have its data keys stripped"
        );
        assert!(
            forged.contains_key("_block_type") && forged.contains_key("_keepme"),
            "system (`_`-prefixed) keys are retained"
        );
    }

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

    #[test]
    fn field_read_no_access_config_allows_all() {
        let lua = setup_lua();
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_allowed_not_in_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                read: Some("test_access.allow".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_denied_in_list() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "secret",
                FieldAccess {
                    read: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
            make_field("title", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_read_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                read: Some("test_access.constrained_string".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert!(denied.is_empty());
    }

    #[test]
    fn field_read_error_counts_as_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "broken",
            FieldAccess {
                read: Some("test_access.throw_error".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(denied, vec![flat("broken")]);
    }

    #[test]
    fn field_read_mixed_access() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "public",
                FieldAccess {
                    read: Some("test_access.allow".into()),
                    ..Default::default()
                },
            ),
            make_field(
                "secret",
                FieldAccess {
                    read: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
            make_field("plain", FieldAccess::default()),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_read_with_user_context() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "admin_only",
            FieldAccess {
                read: Some("test_access.check_user".into()),
                ..Default::default()
            },
        )];

        let admin = make_user_doc("admin");
        let denied = check_field_read_access_with_lua(&lua, &fields, "", Some(&admin), None);
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied = check_field_read_access_with_lua(&lua, &fields, "", Some(&viewer), None);
        assert_eq!(denied, vec![flat("admin_only")]);
    }

    // ── check_field_write_access_with_lua ───────────────────────────────

    #[test]
    fn field_write_no_access_config_allows_all() {
        let lua = setup_lua();
        let fields = vec![make_field("title", FieldAccess::default())];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert!(denied.is_empty());
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "update");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_create_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "locked",
            FieldAccess {
                create: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert_eq!(denied, vec![flat("locked")]);
    }

    #[test]
    fn field_write_update_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "immutable",
            FieldAccess {
                update: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "update");
        assert_eq!(denied, vec![flat("immutable")]);
    }

    #[test]
    fn field_write_create_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.allow".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_unknown_operation_allows() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.deny".into()),
                update: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        // Unknown operation = no extractor = allowed (no restriction).
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "delete");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_error_counts_as_denied() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "broken",
            FieldAccess {
                create: Some("test_access.throw_error".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert_eq!(denied, vec![flat("broken")]);
    }

    #[test]
    fn field_write_create_vs_update_different_access() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "role",
            FieldAccess {
                create: Some("test_access.allow".into()),
                update: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert!(denied.is_empty());

        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "update");
        assert_eq!(denied, vec![flat("role")]);
    }

    #[test]
    fn field_write_constrained_counts_as_allowed() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "status",
            FieldAccess {
                create: Some("test_access.constrained_string".into()),
                ..Default::default()
            },
        )];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert!(denied.is_empty());
    }

    #[test]
    fn field_write_with_user_context() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "admin_only",
            FieldAccess {
                update: Some("test_access.check_user".into()),
                ..Default::default()
            },
        )];
        let admin = make_user_doc("admin");
        let denied =
            check_field_write_access_with_lua(&lua, &fields, "", Some(&admin), None, "update");
        assert!(denied.is_empty());

        let viewer = make_user_doc("viewer");
        let denied =
            check_field_write_access_with_lua(&lua, &fields, "", Some(&viewer), None, "update");
        assert_eq!(denied, vec![flat("admin_only")]);
    }

    // ── recursive field access ────────────────────────────────────────

    #[test]
    fn field_read_recurses_into_group_with_prefix() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "title",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(denied, vec![flat("seo__title")]);
    }

    #[test]
    fn field_read_recurses_through_row_without_prefix() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_write_recurses_into_group() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![make_field(
                    "debug",
                    FieldAccess {
                        create: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert_eq!(denied, vec![flat("config__debug")]);
    }

    #[test]
    fn field_read_recurses_into_array_sub_field() {
        // A denied relationship/upload/scalar inside an array row must be
        // stripped from every row — emitted as a Nested denial keyed by the
        // array's data key.
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(
            denied,
            vec![FieldDenial::Nested {
                array_key: "items".into(),
                array_block_type: None,
                row_path: vec![],
                leaf: "name".into(),
            }]
        );
    }

    #[test]
    fn field_read_recurses_into_group_inside_array() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![make_field(
                            "secret",
                            FieldAccess {
                                read: Some("test_access.deny".into()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(
            denied,
            vec![FieldDenial::Nested {
                array_key: "items".into(),
                array_block_type: None,
                row_path: vec![DenialSeg::Group("meta".into())],
                leaf: "secret".into(),
            }]
        );
    }

    #[test]
    fn field_write_recurses_into_array_in_array() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![make_field(
                            "locked",
                            FieldAccess {
                                create: Some("test_access.deny".into()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert_eq!(
            denied,
            vec![FieldDenial::Nested {
                array_key: "outer".into(),
                array_block_type: None,
                row_path: vec![DenialSeg::Rows {
                    key: "inner".into(),
                    block_type: None
                }],
                leaf: "locked".into(),
            }]
        );
    }

    // ── has_any_field_access ─────────────────────────────────────────

    #[test]
    fn has_any_no_access_configured() {
        let fields = vec![
            make_field("title", FieldAccess::default()),
            make_field("body", FieldAccess::default()),
        ];
        assert!(!has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_top_level_read() {
        let fields = vec![make_field(
            "secret",
            FieldAccess {
                read: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_nested_in_group() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field(
                    "canonical_url",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_nested_in_row() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![make_field(
                    "secret",
                    FieldAccess {
                        create: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.create.as_ref()));
    }

    #[test]
    fn has_any_includes_array_sub_fields() {
        // Field access inside an array IS enforced (nested-JSON stripping).
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![make_field(
                    "name",
                    FieldAccess {
                        read: Some("test_access.deny".into()),
                        ..Default::default()
                    },
                )])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_deeply_nested_group_in_row() {
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("grp", FieldType::Group)
                        .fields(vec![make_field(
                            "deep",
                            FieldAccess {
                                update: Some("test_access.deny".into()),
                                ..Default::default()
                            },
                        )])
                        .build(),
                ])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.update.as_ref()));
    }

    #[test]
    fn field_read_recurses_into_tabs_sub_fields() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab {
                    label: "Main".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "secret",
                        FieldAccess {
                            read: Some("test_access.deny".into()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        let denied = check_field_read_access_with_lua(&lua, &fields, "", None, None);
        assert_eq!(denied, vec![flat("secret")]);
    }

    #[test]
    fn field_write_recurses_into_tabs_sub_fields() {
        let lua = setup_lua();
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab {
                    label: "Settings".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "locked",
                        FieldAccess {
                            create: Some("test_access.deny".into()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        let denied = check_field_write_access_with_lua(&lua, &fields, "", None, None, "create");
        assert_eq!(denied, vec![flat("locked")]);
    }

    #[test]
    fn has_any_nested_in_tabs() {
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab {
                    label: "SEO".to_string(),
                    description: None,
                    fields: vec![make_field(
                        "meta_title",
                        FieldAccess {
                            read: Some("test_access.deny".into()),
                            ..Default::default()
                        },
                    )],
                }])
                .build(),
        ];
        assert!(has_any_field_access(&fields, |f| f.access.read.as_ref()));
    }

    #[test]
    fn has_any_write_checks_correct_extractor() {
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        assert!(!has_any_field_access(&fields, |f| f.access.update.as_ref()));
        assert!(has_any_field_access(&fields, |f| f.access.create.as_ref()));
    }

    // ── Data-aware denial-NAME collectors (the `field_*_denied` introspection
    //    helpers' doc-aware branch) ───────────────────────────────────────────

    fn doc(value: serde_json::Value) -> DocumentFields {
        serde_json::from_value(value).expect("valid document")
    }

    #[test]
    fn collect_write_denied_routes_by_operation() {
        let lua = setup_lua();
        let fields = vec![
            make_field(
                "auto_slug",
                FieldAccess {
                    create: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
            make_field(
                "locked",
                FieldAccess {
                    update: Some("test_access.deny".into()),
                    ..Default::default()
                },
            ),
        ];
        let d = doc(json!({}));

        // create-denied field shows up under "create", not "update".
        let on_create = collect_write_denied_with_lua(&lua, &fields, &d, "", None, None, "create");
        assert_eq!(on_create, vec![flat("auto_slug")]);

        let on_update = collect_write_denied_with_lua(&lua, &fields, &d, "", None, None, "update");
        assert_eq!(on_update, vec![flat("locked")]);
    }

    #[test]
    fn collect_write_denied_unknown_operation_is_empty() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "x",
            FieldAccess {
                create: Some("test_access.deny".into()),
                ..Default::default()
            },
        )];
        let denied =
            collect_write_denied_with_lua(&lua, &fields, &doc(json!({})), "", None, None, "delete");
        assert!(denied.is_empty());
    }

    #[test]
    fn collect_write_denied_is_data_aware() {
        // `check_data` allows the write only when ctx.data.title == "test".
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                create: Some("test_access.check_data".into()),
                ..Default::default()
            },
        )];

        let allowed = collect_write_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "test" })),
            "",
            None,
            None,
            "create",
        );
        assert!(
            allowed.is_empty(),
            "data-aware rule should allow when ctx.data matches"
        );

        let denied = collect_write_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "other" })),
            "",
            None,
            None,
            "create",
        );
        assert_eq!(denied, vec![flat("title")]);
    }

    #[test]
    fn collect_read_denied_is_data_aware() {
        let lua = setup_lua();
        let fields = vec![make_field(
            "title",
            FieldAccess {
                read: Some("test_access.check_data".into()),
                ..Default::default()
            },
        )];

        let allowed = collect_read_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "test" })),
            "",
            None,
            None,
        );
        assert!(allowed.is_empty());

        let denied = collect_read_denied_with_lua(
            &lua,
            &fields,
            &doc(json!({ "title": "x" })),
            "",
            None,
            None,
        );
        assert_eq!(denied, vec![flat("title")]);
    }
}
