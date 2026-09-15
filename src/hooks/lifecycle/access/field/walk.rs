//! Pure field-access tree walkers, parameterized by an `is_denied` predicate so
//! the Lua-evaluated path and the fail-closed deny-all path share one set of
//! container-recursion rules. No Lua here — see `check`/`strip` for the
//! VM-integrated entry points.

use serde_json::{Map, Value};

use crate::core::{
    BLOCK_TYPE_KEY, DenialSeg, DocumentFields, FieldChildren, FieldDefinition, FieldDenial,
    HookRef, any_field, field_children,
};
use crate::db::query::helpers::prefixed_name;

pub(super) fn extract_read_access(f: &FieldDefinition) -> Option<&HookRef> {
    f.access.read.as_ref()
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
            // A companion (a date's zone, a code field's language) is part of
            // its value.
            out.extend(
                field
                    .columns_with_companions(&full_name)
                    .map(FieldDenial::Flat),
            );

            continue; // Parent denied → its sub-fields go with it.
        }

        match field_children(field) {
            FieldChildren::Group(sub) => collect_denials_flat(sub, is_denied, &full_name, out),
            FieldChildren::Wrapper(sub) => collect_denials_flat(sub, is_denied, prefix, out),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_denials_flat(&tab.fields, is_denied, prefix, out);
                }
            }
            FieldChildren::Array(sub) => {
                collect_denials_nested(sub, is_denied, &full_name, None, &[], out);
            }
            FieldChildren::Blocks(blocks) => {
                for block in blocks {
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
            FieldChildren::Leaf => {}
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
            let denial = |leaf: String| FieldDenial::Nested {
                array_key: array_key.to_string(),
                array_block_type: array_block_type.map(str::to_string),
                row_path: row_path.to_vec(),
                leaf,
            };
            // A companion (a date's zone, a code field's language) is part of
            // its value.
            out.extend(field.columns_with_companions(&field.name).map(denial));

            continue;
        }

        match field_children(field) {
            FieldChildren::Group(sub) => {
                let mut path = row_path.to_vec();
                path.push(DenialSeg::Group(field.name.clone()));
                collect_denials_nested(sub, is_denied, array_key, array_block_type, &path, out);
            }
            FieldChildren::Wrapper(sub) => {
                collect_denials_nested(sub, is_denied, array_key, array_block_type, row_path, out);
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
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
            FieldChildren::Array(sub) => {
                let mut path = row_path.to_vec();
                path.push(DenialSeg::Rows {
                    key: field.name.clone(),
                    block_type: None,
                });
                collect_denials_nested(sub, is_denied, array_key, array_block_type, &path, out);
            }
            FieldChildren::Blocks(blocks) => {
                // One `Rows` step per block type so the strip only touches rows
                // of that type — a denial in one block must not strip a
                // same-named field from a sibling block type.
                for block in blocks {
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
            FieldChildren::Leaf => {}
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
    // Snapshot the current level once for `ctx.data` (read-only sibling view);
    // every field at this level — including those wrapped in a transparent
    // Row/Collapsible/Tabs — evaluates against this same pre-strip view, so a
    // field's strip decision never depends on its layout placement. The real
    // map is mutated (denied fields removed) as we go.
    let snapshot: DocumentFields = level.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

    strip_level_with_snapshot(fields, level, &snapshot, extract, is_denied);
}

/// Inner worker for [`strip_access_data_aware`]: strips one document level
/// against an already-computed `snapshot` (the `ctx.data` sibling view).
///
/// Kept separate from the public entry so transparent layout wrappers
/// (Row/Collapsible/Tabs) — which do NOT introduce a new document level —
/// forward the *parent* level's snapshot, making a wrapped field evaluate
/// `ctx.data` identically to a direct sibling. Genuine new levels (a Group's
/// object, Array/Blocks rows) re-enter via [`strip_access_data_aware`] so they
/// snapshot their own level.
fn strip_level_with_snapshot<E, F>(
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    snapshot: &DocumentFields,
    extract: &E,
    is_denied: &F,
) where
    E: Fn(&FieldDefinition) -> Option<&HookRef>,
    F: Fn(&HookRef, &DocumentFields) -> bool,
{
    for field in fields {
        if let Some(hook) = extract(field)
            && is_denied(hook, snapshot)
        {
            // A companion (a date's zone, a code field's language) is part of
            // its value.
            for column in field.columns_with_companions(&field.name) {
                level.remove(&column);
            }

            continue; // Parent denied → its sub-fields go with it.
        }

        match field_children(field) {
            FieldChildren::Group(sub) => {
                if let Some(Value::Object(obj)) = level.get_mut(&field.name) {
                    strip_access_data_aware(sub, obj, extract, is_denied);
                }
            }
            FieldChildren::Wrapper(sub) => {
                strip_level_with_snapshot(sub, level, snapshot, extract, is_denied);
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    strip_level_with_snapshot(&tab.fields, level, snapshot, extract, is_denied);
                }
            }
            FieldChildren::Array(sub) => {
                if let Some(Value::Array(rows)) = level.get_mut(&field.name) {
                    for row in rows.iter_mut() {
                        if let Value::Object(r) = row {
                            strip_access_data_aware(sub, r, extract, is_denied);
                        }
                    }
                }
            }
            FieldChildren::Blocks(blocks) => {
                if let Some(Value::Array(rows)) = level.get_mut(&field.name) {
                    for row in rows.iter_mut() {
                        let Value::Object(r) = row else { continue };
                        let block_type = r
                            .get(BLOCK_TYPE_KEY)
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if let Some(block) = blocks.iter().find(|b| b.block_type == block_type) {
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
            FieldChildren::Leaf => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::test_helpers::*;
    use super::*;
    use crate::core::{
        BlockDefinition, FieldAccess, FieldAdmin, FieldDefinition, FieldTab, FieldType,
    };
    use serde_json::json;

    /// A code field with a language allow-list, gated by `access`.
    fn code_lang_field(access: FieldAccess) -> FieldDefinition {
        FieldDefinition::builder("snippet", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .access(access)
            .build()
    }

    fn object(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("fixture is an object: {value}");
        };

        map
    }

    /// Regression: a denied code field's `<name>_lang` companion wasn't denied
    /// with it, so a hidden or read-denied code field still shipped its
    /// language — at the document level, in groups, and inside array rows.
    #[test]
    fn denying_a_code_field_denies_its_language() {
        let code = code_lang_field(FieldAccess::default());
        let fields = vec![
            code.clone(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![code.clone()])
                .build(),
            FieldDefinition::builder("slots", FieldType::Array)
                .fields(vec![code])
                .build(),
        ];
        let mut out = Vec::new();

        collect_denials_flat(
            &fields,
            &|f: &FieldDefinition| f.name == "snippet",
            "",
            &mut out,
        );

        let paths: Vec<String> = out.iter().map(FieldDenial::display_path).collect();
        for path in [
            "snippet",
            "snippet_lang",
            "seo__snippet",
            "seo__snippet_lang",
            "slots.snippet",
            "slots.snippet_lang",
        ] {
            assert!(paths.contains(&path.to_string()), "{path}: {paths:?}");
        }
    }

    /// Regression: a read-denied code field was removed but its `<name>_lang`
    /// companion stayed in the document — at the top level and inside a group.
    #[test]
    fn a_read_denied_code_field_takes_its_language_with_it() {
        let gated = code_lang_field(FieldAccess {
            read: Some("h".into()),
            ..Default::default()
        });
        let fields = vec![
            gated.clone(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![gated])
                .build(),
        ];
        let mut level = object(json!({
            "snippet": "print(1)",
            "snippet_lang": "python",
            "meta": { "snippet": "x", "snippet_lang": "javascript", "kept": 1 },
            "title": "kept",
        }));

        strip_read_access_data_aware(&fields, &mut level, &|_, _| true);

        assert!(!level.contains_key("snippet"), "{level:?}");
        assert!(!level.contains_key("snippet_lang"), "{level:?}");
        assert!(level.contains_key("title"), "{level:?}");

        let meta = level["meta"].as_object().unwrap();
        assert!(!meta.contains_key("snippet"), "{meta:?}");
        assert!(!meta.contains_key("snippet_lang"), "{meta:?}");
        assert!(meta.contains_key("kept"), "{meta:?}");
    }

    /// Regression: a write-denied code field's value was dropped from the write
    /// data but its `<name>_lang` companion was not, so the language stayed
    /// writable through the companion key.
    #[test]
    fn a_write_denied_code_field_drops_its_language_from_the_write() {
        let fields = vec![code_lang_field(FieldAccess {
            update: Some("h".into()),
            ..Default::default()
        })];
        let mut data = object(json!({
            "snippet": "print(1)",
            "snippet_lang": "python",
        }));

        strip_access_data_aware(
            &fields,
            &mut data,
            &|f| f.access.update.as_ref(),
            &|_, _| true,
        );

        assert!(data.is_empty(), "{data:?}");
    }

    fn read_gated(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(FieldAccess {
                read: Some("h".into()),
                ..Default::default()
            })
            .build()
    }

    /// Like [`read_gated`] but with an explicit hook reference, so a mock
    /// `is_denied` can branch per field.
    fn read_gated_with(name: &str, hook: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .access(FieldAccess {
                read: Some(hook.into()),
                ..Default::default()
            })
            .build()
    }

    /// Regression: a denied timezone date's `<name>_tz` companion wasn't denied
    /// with it, so a hidden or read-denied date still shipped its zone — at the
    /// document level, in groups, and inside array rows.
    #[test]
    fn denying_a_timezone_date_denies_its_zone() {
        let date = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();
        let fields = vec![
            date.clone(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![date.clone()])
                .build(),
            FieldDefinition::builder("slots", FieldType::Array)
                .fields(vec![date])
                .build(),
        ];
        let mut out = Vec::new();

        collect_denials_flat(
            &fields,
            &|f: &FieldDefinition| f.name == "starts",
            "",
            &mut out,
        );

        let paths: Vec<String> = out.iter().map(FieldDenial::display_path).collect();
        for path in [
            "starts",
            "starts_tz",
            "seo__starts",
            "seo__starts_tz",
            "slots.starts",
            "slots.starts_tz",
        ] {
            assert!(paths.contains(&path.to_string()), "{path}: {paths:?}");
        }
    }

    /// Regression: a read-denied timezone date was removed but its `<name>_tz`
    /// companion stayed in the document, leaking the zone of a value the
    /// reader may not see.
    #[test]
    fn a_denied_timezone_date_takes_its_zone_with_it() {
        let fields = vec![
            FieldDefinition::builder("starts", FieldType::Date)
                .timezone(true)
                .access(FieldAccess {
                    read: Some("h".into()),
                    ..Default::default()
                })
                .build(),
        ];
        let mut level = json!({
            "starts": "2026-01-01T10:00:00.000Z",
            "starts_tz": "Europe/Berlin",
            "title": "kept",
        })
        .as_object()
        .unwrap()
        .clone();

        strip_read_access_data_aware(&fields, &mut level, &|_, _| true);

        assert!(!level.contains_key("starts"), "{level:?}");
        assert!(!level.contains_key("starts_tz"), "{level:?}");
        assert!(level.contains_key("title"), "{level:?}");
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

    /// Layout wrappers (Row/Collapsible/Tabs) are transparent: a field wrapped
    /// in one must evaluate `ctx.data` against the SAME pre-strip sibling
    /// snapshot as a direct sibling. Regression for the wrapper-re-snapshot bug
    /// where the Row recursion re-snapshotted the level *after* a preceding
    /// sibling had already been stripped, flipping the strip decision purely
    /// based on placement.
    #[test]
    fn data_aware_strip_transparent_wrapper_shares_sibling_snapshot() {
        // `secret` is always denied; `dependent` is kept only while `secret` is
        // still present in the sibling view (`ctx.data`).
        let is_denied = |hook: &HookRef, level: &DocumentFields| match hook.reference() {
            "always" => true,
            "needs_secret" => level.get_str("secret").is_none(),
            _ => false,
        };

        // (a) `secret` and `dependent` as direct siblings.
        let direct = vec![
            read_gated_with("secret", "always"),
            read_gated_with("dependent", "needs_secret"),
        ];
        let mut d = json!({ "secret": "s", "dependent": "d" })
            .as_object()
            .unwrap()
            .clone();
        strip_read_access_data_aware(&direct, &mut d, &is_denied);
        assert!(!d.contains_key("secret"), "secret always stripped");
        let direct_kept = d.contains_key("dependent");

        // (b) same fields + data, but `dependent` wrapped in a transparent Row.
        let wrapped = vec![
            read_gated_with("secret", "always"),
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![read_gated_with("dependent", "needs_secret")])
                .build(),
        ];
        let mut w = json!({ "secret": "s", "dependent": "d" })
            .as_object()
            .unwrap()
            .clone();
        strip_read_access_data_aware(&wrapped, &mut w, &is_denied);
        let wrapped_kept = w.contains_key("dependent");

        assert_eq!(
            direct_kept, wrapped_kept,
            "transparent Row changed the strip decision \
             (direct kept={direct_kept}, row-wrapped kept={wrapped_kept})"
        );
        assert!(
            direct_kept,
            "`dependent` must see the pre-strip `secret` sibling → kept in both layouts"
        );
    }

    /// Fail-closed: a Blocks row whose `_block_type` resolves to no block
    /// definition (forged/missing type) must NOT pass its data through
    /// unstripped. The walker can't map the row's keys to any field-access rule,
    /// so it drops every non-system key, keeping only `_`-prefixed metadata.
    #[test]
    fn data_aware_strip_drops_unresolved_block_row_data() {
        let fields = vec![
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
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
}
