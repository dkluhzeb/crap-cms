//! Draft version save: merge data onto existing doc, snapshot, prune.

use std::collections::HashSet;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{
        Document, DocumentFields, FieldChildren, FieldDefinition, FieldType,
        collection::VersionsConfig, field_children, flatten_group_fields, nest_group_fields,
        walk_leaf_fields,
    },
    db::{
        DbConnection, LocaleContext, query,
        query::{
            helpers::{locale_column, prefixed_name, tz_column},
            locale_locked_field_names,
        },
    },
};

use super::snapshot::prune_versions;

/// Inputs for [`save_draft_version`]. All fields are required; constructed at
/// each draft-save site (collection persist, global persist, test).
pub(crate) struct SaveDraftArgs<'a> {
    pub conn: &'a dyn DbConnection,
    pub table: &'a str,
    pub parent_id: &'a str,
    pub fields: &'a [FieldDefinition],
    pub versions: Option<&'a VersionsConfig>,
    pub existing_doc: &'a Document,
    pub data: &'a DocumentFields,
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// Save a draft-only version: merge incoming hook-processed data onto the
/// latest draft (or, without one, the existing doc), create a version snapshot,
/// and prune.
///
/// Returns the stored snapshot — the DRAFT content. Callers hand that back to
/// hooks, the response, and the event, so a draft save reports what was
/// written rather than the untouched published row.
pub(crate) fn save_draft_version(args: &SaveDraftArgs<'_>) -> Result<Value> {
    let SaveDraftArgs {
        conn,
        table,
        parent_id,
        fields,
        versions,
        existing_doc: _,
        data: final_ctx_data,
        locale_ctx,
    } = *args;

    // Locale-locked shared fields must not enter the snapshot from a non-default
    // locale edit — otherwise the canonical default-locale value would be
    // overwritten on restore. Mirrors the published UPDATE path's locale-lock.
    let locked = locale_locked_field_names(fields, locale_ctx);

    // The existing document carries flat group columns (`seo__title`), but the
    // incoming hook data can arrive with nested group objects (`seo: { title }`)
    // from the gRPC/MCP/admin surfaces. Flatten before overlaying so an edited
    // sub-field overwrites the matching flat key instead of leaving a duplicate
    // nested object behind.
    let mut flattened = flatten_group_fields(final_ctx_data, fields);

    // Drop locale-locked fields (scalar columns AND join fields) so neither the
    // overlay nor the join re-merge can bake a non-default-locale edit of a
    // shared field into the snapshot.
    flattened.retain(|k, _| !locked.contains(k));

    // A localized join field's edit belongs to the saving locale only: it is
    // written under that locale's snapshot key, never over the rows the other
    // locales keep.
    let localized_joins = localized_join_edits(fields, locale_ctx);
    let mut overlay = flattened.clone();
    overlay.retain(|k, _| !localized_joins.contains(k));

    // A pending draft is the base of the next draft save, so a second save — in
    // another locale, or sending a single field — keeps the earlier edits. Only
    // without one does the draft start from the stored document.
    let prior_draft = query::find_latest_version(conn, table, parent_id)?
        .filter(|v| v.status == "draft")
        .and_then(|v| v.snapshot.as_object().cloned());

    let mut snapshot = match prior_draft {
        Some(prior) => overlay_on_draft(prior, fields, &overlay, locale_ctx)?,
        None => snapshot_from_stored(args, &overlay)?,
    };

    if let Some(obj) = snapshot.as_object_mut() {
        merge_join_data_into_snapshot(obj, fields, &overlay);

        if let Some(ctx) = locale_ctx.filter(|c| c.config.is_enabled()) {
            merge_localized_join_rows(obj, &localized_joins, &flattened, ctx)?;
        }

        stamp_write_locale_columns(obj, fields, &overlay, locale_ctx)?;
    }

    query::create_version(conn, table, parent_id, "draft", &snapshot)?;

    prune_versions(conn, table, parent_id, versions)?;

    Ok(snapshot)
}

/// Flat keys of the localized join fields, when this save is locale-scoped.
fn localized_join_edits(
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Vec<String> {
    if locale_ctx.is_some_and(|c| c.config.is_enabled()) {
        query::localized_join_keys(fields)
    } else {
        Vec::new()
    }
}

/// The first draft of a stored document: the document with the edit overlaid,
/// snapshotted with every locale's columns and join rows read from the database.
/// `build_snapshot` rebuilds join data from the DB — the pre-edit state — so the
/// caller re-overlays the edited join values afterwards.
fn snapshot_from_stored(args: &SaveDraftArgs<'_>, overlay: &DocumentFields) -> Result<Value> {
    let existing_doc = args.existing_doc;
    let mut snapshot_fields = existing_doc.fields.clone();

    for (k, v) in overlay {
        snapshot_fields.insert(k.clone(), v.clone());
    }

    let snapshot_doc = Document::builder(args.parent_id)
        .fields(snapshot_fields)
        .created_at(existing_doc.created_at.as_deref())
        .updated_at(existing_doc.updated_at.as_deref())
        .build();

    query::build_snapshot(
        args.conn,
        args.table,
        args.fields,
        &snapshot_doc,
        args.locale_ctx.map(|c| &c.config),
    )
}

/// Overlay the edit on the latest draft snapshot. Snapshots store groups nested
/// while the edit is flat, so the snapshot is flattened, overlaid and nested
/// again — an edited group sub-field replaces its value instead of sitting
/// beside a stale nested one. Every other key (the other locales' columns and
/// rows, fields the edit did not send) keeps its drafted value.
fn overlay_on_draft(
    prior: Map<String, Value>,
    fields: &[FieldDefinition],
    overlay: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Value> {
    let (prior, per_locale) = split_per_locale_keys(prior, fields, locale_ctx)?;
    let mut flat = flatten_group_fields(&prior, fields);

    for (k, v) in overlay {
        flat.insert(k.clone(), v.clone());
    }

    let mut snapshot: Map<String, Value> = nest_group_fields(&flat, fields)
        .into_inner()
        .into_iter()
        .collect();
    snapshot.extend(per_locale);

    Ok(Value::Object(snapshot))
}

/// Split off a draft snapshot's per-locale keys (`title__de`,
/// `seo__title__de`, `gallery__slides__de`). They live flat at the snapshot
/// root, where the draft read and restore look them up; nesting groups would
/// move a group field's keys into the group object.
fn split_per_locale_keys(
    prior: Map<String, Value>,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<(DocumentFields, Map<String, Value>)> {
    let Some(ctx) = locale_ctx.filter(|c| c.config.is_enabled()) else {
        return Ok((prior.into_iter().collect(), Map::new()));
    };

    let mut per_locale_keys = HashSet::new();
    for base in per_locale_bases(fields) {
        for locale in &ctx.config.locales {
            per_locale_keys.insert(locale_column(&base, locale)?);
        }
    }

    let mut rest = DocumentFields::new();
    let mut per_locale = Map::new();

    for (key, value) in prior {
        if per_locale_keys.contains(&key) {
            per_locale.insert(key, value);
        } else {
            rest.insert(key, value);
        }
    }

    Ok((rest, per_locale))
}

/// The flat keys a snapshot records per locale: localized columns — with a
/// timezone date's companion — and localized join fields.
fn per_locale_bases(fields: &[FieldDefinition]) -> HashSet<String> {
    let mut bases: HashSet<String> = query::localized_join_keys(fields).into_iter().collect();

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
        if (field.localized || inherited) && field.has_parent_column() {
            let name = prefixed_name(prefix, &field.name);

            if field.has_tz_companion() {
                bases.insert(tz_column(&name));
            }
            bases.insert(name);
        }

        Ok(())
    });

    bases
}

/// Write the edited rows of each localized join field under the saving locale's
/// snapshot key (`{key}__{locale}`, the locale code in column form), and under a
/// top-level field's bare key when saving the default locale. The other
/// locales' keys keep their rows.
fn merge_localized_join_rows(
    obj: &mut Map<String, Value>,
    keys: &[String],
    data: &DocumentFields,
    ctx: &LocaleContext,
) -> Result<()> {
    let locale = ctx.access_locale();

    for key in keys {
        let Some(rows) = data.get(key) else {
            continue;
        };

        obj.insert(locale_column(key, locale)?, rows.clone());

        // Field names cannot contain `__`, so a key without it is top-level and
        // its bare key sits at the snapshot root.
        if locale == ctx.config.default_locale && !key.contains("__") {
            obj.insert(key.clone(), rows.clone());
        }
    }

    Ok(())
}

/// Write the draft's own value into the per-locale column for the locale the
/// draft was saved under.
///
/// Same reason the join data is re-overlaid: `build_snapshot` reads the
/// decorated `title__xx` columns straight from the main table, which a draft
/// save never touches. Only fields this save sent are stamped: an unsent field
/// keeps its drafted per-locale value, and the bare key may hold another
/// locale's edit. Join fields carry their per-locale rows separately (see
/// [`merge_localized_join_rows`]).
fn stamp_write_locale_columns(
    snapshot: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let Some(ctx) = locale_ctx.filter(|c| c.config.is_enabled()) else {
        return Ok(());
    };
    let locale = ctx.access_locale();

    walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
        if !(field.localized || inherited) || !field.has_parent_column() {
            return Ok(());
        }

        let name = prefixed_name(prefix, &field.name);
        if let Some(value) = data.get(&name) {
            snapshot.insert(locale_column(&name, locale)?, value.clone());
        }

        // A timezone date's zone is stamped beside it: the draft read resolves
        // the companion from its per-locale key too.
        let tz = tz_column(&name);
        if field.has_tz_companion()
            && let Some(value) = data.get(&tz)
        {
            snapshot.insert(locale_column(&tz, locale)?, value.clone());
        }

        Ok(())
    })
}

/// Overlay join-table data (arrays, blocks, has-many relationships) from the
/// flattened incoming edit onto the snapshot. Handles join fields at the top
/// level, nested inside a Group (written into the snapshot's nested group
/// object), and inside transparent Tabs/Row/Collapsible wrappers. The flattened
/// data keys a group child as `group__child`.
fn merge_join_data_into_snapshot(
    obj: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    data: &DocumentFields,
) {
    merge_join_data_prefixed(obj, fields, data, "");
}

/// Inner recursion carrying the group prefix used to look the join value up in
/// the flattened `data` (`""` at the top level, `group` / `group__sub` deeper).
fn merge_join_data_prefixed(
    obj: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    prefix: &str,
) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Array(_) | FieldChildren::Blocks(_) => {
                let key = prefixed_name(prefix, &field.name);

                if let Some(v) = data.get(&key) {
                    obj.insert(field.name.clone(), v.clone());
                }
            }
            FieldChildren::Group(sub) => {
                let group_prefix = prefixed_name(prefix, &field.name);

                // The snapshot stores the group as a nested object; merge the
                // group's join children into it (or build it if absent, but only
                // when the edit actually carries group-nested join data).
                if let Some(Value::Object(group_obj)) = obj.get_mut(&field.name) {
                    merge_join_data_prefixed(group_obj, sub, data, &group_prefix);
                } else {
                    let mut group_obj = Map::new();
                    merge_join_data_prefixed(&mut group_obj, sub, data, &group_prefix);

                    if !group_obj.is_empty() {
                        obj.insert(field.name.clone(), Value::Object(group_obj));
                    }
                }
            }
            FieldChildren::Wrapper(sub) => {
                merge_join_data_prefixed(obj, sub, data, prefix);
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    merge_join_data_prefixed(obj, &tab.fields, data, prefix);
                }
            }
            // A has-many Relationship or Upload stores its join rows in the
            // snapshot the same way Array/Blocks do; other leaves carry no join
            // data here.
            FieldChildren::Leaf => {
                if matches!(
                    field.field_type,
                    FieldType::Relationship | FieldType::Upload
                ) {
                    let key = prefixed_name(prefix, &field.name);

                    if let Some(v) = data.get(&key) {
                        obj.insert(field.name.clone(), v.clone());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{config::LocaleConfig, core::field::RelationshipConfig, db::LocaleMode};

    use super::*;

    /// Regression: overlaying an edit on a draft nested the per-locale keys of
    /// a field inside a group (`seo__title__en`) into the group object, so the
    /// draft read in one locale found another locale's edit and a restore
    /// missed the group's localized rows. Per-locale keys stay at the root.
    #[test]
    fn per_locale_keys_of_a_group_field_stay_at_the_snapshot_root() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .localized(true)
                        .build(),
                    FieldDefinition::builder("slides", FieldType::Array)
                        .localized(true)
                        .build(),
                ])
                .build(),
        ];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };
        let prior = json!({
            "seo": { "title": "A" },
            "seo__title__en": "A",
            "seo__slides__en": [{ "id": "r1" }],
        })
        .as_object()
        .unwrap()
        .clone();

        let mut overlay = DocumentFields::new();
        overlay.insert("seo__title".into(), json!("B"));

        let snapshot = overlay_on_draft(prior, &fields, &overlay, Some(&ctx)).unwrap();

        assert_eq!(snapshot["seo__title__en"], json!("A"), "{snapshot}");
        assert_eq!(
            snapshot["seo__slides__en"],
            json!([{ "id": "r1" }]),
            "{snapshot}"
        );
        assert_eq!(snapshot["seo"], json!({ "title": "B" }), "{snapshot}");
    }

    /// A hyphenated locale's per-locale snapshot keys take the column form
    /// (`title__pt_BR`) — the form `build_snapshot` records and the draft read
    /// and restore look up. The draft save wrote `title__pt-BR`, which nothing
    /// reads, and didn't recognise `title__pt_BR` as per-locale on a later save.
    #[test]
    fn a_hyphenated_locale_uses_the_column_form_of_its_snapshot_keys() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .localized(true)
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("slides", FieldType::Array)
                .localized(true)
                .build(),
        ];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("pt-BR".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "pt-BR".to_string()],
                fallback: false,
            },
        };

        let prior = json!({ "seo": { "title": "A" }, "seo__title__pt_BR": "A" })
            .as_object()
            .unwrap()
            .clone();
        let mut edit = DocumentFields::new();
        edit.insert("seo__title".into(), json!("B"));
        edit.insert("slides".into(), json!([{ "id": "r1" }]));

        let mut overlay = edit.clone();
        overlay.remove("slides");
        let snapshot = overlay_on_draft(prior, &fields, &overlay, Some(&ctx)).unwrap();
        let mut obj = snapshot.as_object().unwrap().clone();
        assert_eq!(obj["seo__title__pt_BR"], json!("A"), "{snapshot}");

        stamp_write_locale_columns(&mut obj, &fields, &overlay, Some(&ctx)).unwrap();
        merge_localized_join_rows(&mut obj, &["slides".to_string()], &edit, &ctx).unwrap();

        assert_eq!(obj["seo__title__pt_BR"], json!("B"), "{obj:?}");
        assert_eq!(obj["slides__pt_BR"], json!([{ "id": "r1" }]), "{obj:?}");
        assert!(
            !obj.keys().any(|k| k.contains('-')),
            "no key names the locale code itself: {obj:?}"
        );
    }

    /// Regression: a draft save stamped a localized timezone date's per-locale
    /// value but not its zone's, so the draft read — which resolves the zone
    /// from its per-locale key — showed the stored zone instead of the edit.
    #[test]
    fn a_localized_timezone_date_stamps_its_zone_under_the_saving_locale() {
        let fields = vec![
            FieldDefinition::builder("starts", FieldType::Date)
                .timezone(true)
                .localized(true)
                .build(),
        ];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };
        let mut snapshot = json!({ "starts_tz__de": "Europe/Berlin" })
            .as_object()
            .unwrap()
            .clone();
        let mut edit = DocumentFields::new();
        edit.insert("starts".into(), json!("2026-01-01T10:00:00.000Z"));
        edit.insert("starts_tz".into(), json!("America/New_York"));

        stamp_write_locale_columns(&mut snapshot, &fields, &edit, Some(&ctx)).unwrap();

        assert_eq!(snapshot["starts__de"], json!("2026-01-01T10:00:00.000Z"));
        assert_eq!(snapshot["starts_tz__de"], json!("America/New_York"));
    }

    #[test]
    fn copies_join_field_values_recurses_layout_ignores_scalars_and_groups() {
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            FieldDefinition::builder("items", FieldType::Array).build(),
            FieldDefinition::builder("title", FieldType::Text).build(), // scalar → ignored
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("rows", FieldType::Blocks).build(),
                ])
                .build(),
            // A group's join child is merged only via its flattened
            // `group__child` key — a bare top-level key of the same name is not
            // picked up (see `merges_group_nested_join_field_into_nested_object`
            // for the positive case).
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("ignored", FieldType::Array).build(),
                ])
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("tags".into(), json!(["t1"]));
        data.insert("items".into(), json!([{ "x": 1 }]));
        data.insert("title".into(), json!("Hello"));
        data.insert("rows".into(), json!([{ "_block_type": "hero" }]));
        data.insert("ignored".into(), json!([{ "y": 2 }]));

        let mut obj = serde_json::Map::new();
        merge_join_data_into_snapshot(&mut obj, &fields, &data);

        assert_eq!(obj.get("tags"), Some(&json!(["t1"])));
        assert_eq!(obj.get("items"), Some(&json!([{ "x": 1 }])));
        assert_eq!(obj.get("rows"), Some(&json!([{ "_block_type": "hero" }]))); // via Row
        assert!(!obj.contains_key("title")); // scalar
        assert!(!obj.contains_key("ignored")); // group child needs the `meta__ignored` key
        assert!(!obj.contains_key("meta")); // no group-nested join data supplied
    }

    #[test]
    fn absent_data_keys_are_skipped() {
        let fields = vec![FieldDefinition::builder("items", FieldType::Array).build()];
        let mut obj = serde_json::Map::new();
        merge_join_data_into_snapshot(&mut obj, &fields, &DocumentFields::new());
        assert!(obj.is_empty());
    }

    /// Regression: a has-many Upload is join-table-backed exactly like a
    /// has-many Relationship, so an edit to it must be overlaid onto the
    /// snapshot. `build_snapshot` rebuilds the pre-edit join data from the DB,
    /// so skipping Upload here silently kept the stale selection — the draft
    /// dropped the user's edit.
    #[test]
    fn overlays_edited_has_many_upload_join_value() {
        let fields = vec![
            FieldDefinition::builder("gallery", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", true))
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("gallery".into(), json!(["m1", "m2"]));

        // Snapshot as `build_snapshot` produced it: the stale pre-edit selection.
        let mut obj = serde_json::Map::new();
        obj.insert("gallery".into(), json!(["m0"]));

        merge_join_data_into_snapshot(&mut obj, &fields, &data);

        assert_eq!(
            obj.get("gallery"),
            Some(&json!(["m1", "m2"])),
            "an edited has-many Upload must overlay the snapshot like a Relationship"
        );
    }

    /// Regression: a join field (array/blocks/has-many) nested inside a Group is
    /// merged into the snapshot's nested group object, keyed by its flattened
    /// `group__child` name. `build_snapshot` rebuilds the group's join data from
    /// the DB (pre-edit), so without this the draft edit was silently dropped.
    #[test]
    fn merges_group_nested_join_field_into_nested_object() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("things", FieldType::Array).build(),
                ])
                .build(),
        ];

        // Incoming edit, flattened (as `save_draft_version` flattens before merge).
        let mut data = DocumentFields::new();
        data.insert("meta__things".into(), json!([{ "k": 1 }]));

        // Snapshot as `build_snapshot` produced it: the group is nested, its
        // join child holding the stale (empty) DB data.
        let mut obj = serde_json::Map::new();
        obj.insert("meta".into(), json!({ "things": [] }));

        merge_join_data_into_snapshot(&mut obj, &fields, &data);

        assert_eq!(
            obj["meta"]["things"],
            json!([{ "k": 1 }]),
            "the edited group-nested join field must win over the stale DB data"
        );
    }

    /// Regression: a draft update that edits a group sub-field via the nested
    /// data shape (`{ seo: { title } }`, as gRPC/MCP/admin forms send) must not
    /// leave the stale flat `seo__title` from the existing document in the
    /// snapshot. The existing doc carries flat group columns (`seo__title`);
    /// overlaying the incoming nested object without flattening first left BOTH
    /// keys in the snapshot, and `extract_snapshot_recursive` reads the flat one
    /// first (via `or_insert`), so the edit was silently lost on restore.
    #[test]
    fn draft_overlay_flattens_nested_group_subfield() {
        use crate::db::InMemoryConn;

        let conn = InMemoryConn::open();
        conn.execute_batch(
            "CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT,
                created_at TEXT
            );",
        )
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        // Existing document as read from the DB: flat group column.
        let mut existing_fields = DocumentFields::new();
        existing_fields.insert("seo__title".into(), json!("old"));
        let existing_doc = Document::builder("p1").fields(existing_fields).build();

        // Incoming draft edit in the nested shape (gRPC/MCP/admin form).
        let mut incoming = DocumentFields::new();
        incoming.insert("seo".into(), json!({ "title": "new" }));

        save_draft_version(&SaveDraftArgs {
            conn: &conn,
            table: "posts",
            parent_id: "p1",
            fields: &fields,
            versions: None,
            existing_doc: &existing_doc,
            data: &incoming,
            locale_ctx: None,
        })
        .unwrap();

        let versions = query::list_versions(&conn, "posts", "p1", false, None, None).unwrap();
        assert_eq!(versions.len(), 1);

        // `build_snapshot` hydrates flat group columns back into nested form,
        // so the canonical snapshot shape for a group is `seo: { title }`. The
        // edit must be the value that survives.
        let snapshot = &versions[0].snapshot;
        assert_eq!(
            snapshot.pointer("/seo/title"),
            Some(&json!("new")),
            "the draft edit must win over the stale existing value"
        );
    }
}
