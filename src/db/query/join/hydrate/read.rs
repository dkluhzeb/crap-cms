//! Document hydration — populates join-table fields (arrays, blocks, relationships)
//! into documents after the main row query.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use super::{
    super::{
        arrays::{find_array_rows, find_array_rows_batch},
        blocks::{find_block_rows, find_block_rows_batch},
        relationships::{
            find_polymorphic_related, find_polymorphic_related_batch, find_related_ids,
            find_related_ids_batch,
        },
    },
    group::reconstruct_group_fields,
    locale,
};
use crate::{
    core::{Document, FieldDefinition, FieldType, field::RelationshipConfig},
    db::{
        DbConnection, LocaleContext,
        query::{
            helpers::{parse_has_many_scalar, prefixed_name, walk_leaf_fields},
            poly_ref,
        },
    },
};

/// Parse scalar has-many columns back into typed JSON arrays, in place.
///
/// `row_to_document` is field-type-blind, so a scalar has-many column (a JSON
/// array stored as TEXT — see [`FieldDefinition::is_has_many_scalar`]) arrives
/// here as a raw string. This runs before group reconstruction / locale
/// reshaping, so it only ever sees flat columns: the bare column plus its
/// per-locale `{col}__{locale}` variants (matched by prefix). Downstream steps
/// then relocate the already-typed arrays. Array/blocks rows are untouched —
/// `walk_leaf_fields` visits them as leaves without descending, and their nested
/// lists are parsed by array/block hydration instead.
fn parse_has_many_scalar_columns(fields: &[FieldDefinition], doc: &mut Document) {
    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        if !field.is_has_many_scalar() {
            return Ok(());
        }

        let base = prefixed_name(prefix, &field.name);
        let locale_prefix = format!("{base}__");
        let keys: Vec<String> = doc
            .fields
            .keys()
            .filter(|k| *k == &base || k.starts_with(&locale_prefix))
            .cloned()
            .collect();

        for k in keys {
            if let Some(v) = doc.fields.get(&k) {
                let parsed = parse_has_many_scalar(&field.field_type, v);
                doc.fields.insert(k, parsed);
            }
        }

        Ok(())
    });
}

/// Hydrate a has-many relationship field, returning the JSON array value.
/// Handles both polymorphic and non-polymorphic relationships with locale fallback.
fn hydrate_relationship(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    doc_id: &str,
    rc: &RelationshipConfig,
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<Value> {
    if rc.is_polymorphic() {
        let mut items = find_polymorphic_related(conn, slug, field_name, doc_id, locale_ref)?;

        if items.is_empty() && fallback_ref.is_some() {
            items = find_polymorphic_related(conn, slug, field_name, doc_id, fallback_ref)?;
        }

        let json_items: Vec<Value> = items
            .into_iter()
            .map(|(col, id)| Value::String(poly_ref::format(&col, &id)))
            .collect();

        Ok(Value::Array(json_items))
    } else {
        let mut ids = find_related_ids(conn, slug, field_name, doc_id, locale_ref)?;

        if ids.is_empty() && fallback_ref.is_some() {
            ids = find_related_ids(conn, slug, field_name, doc_id, fallback_ref)?;
        }

        let json_ids: Vec<Value> = ids.into_iter().map(Value::String).collect();

        Ok(Value::Array(json_ids))
    }
}

/// Hydrate an array field, returning the JSON array value with locale fallback.
fn hydrate_array(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    doc_id: &str,
    sub_fields: &[FieldDefinition],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<Value> {
    let mut rows = find_array_rows(conn, slug, field_name, doc_id, sub_fields, locale_ref)?;

    if rows.is_empty() && fallback_ref.is_some() {
        rows = find_array_rows(conn, slug, field_name, doc_id, sub_fields, fallback_ref)?;
    }

    Ok(Value::Array(rows))
}

/// Hydrate a blocks field, returning the JSON array value with locale fallback.
fn hydrate_blocks(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    doc_id: &str,
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<Value> {
    let mut rows = find_block_rows(conn, slug, field_name, doc_id, locale_ref)?;

    if rows.is_empty() && fallback_ref.is_some() {
        rows = find_block_rows(conn, slug, field_name, doc_id, fallback_ref)?;
    }

    Ok(Value::Array(rows))
}

/// Recursively hydrate join-table types (Array, Blocks, Relationship) inside a Group.
/// Uses `__`-prefixed names for join table lookups (e.g., `profile__skills` → table
/// `{collection}_profile__skills`). Results are inserted into `group_obj` under bare field names.
fn hydrate_group_join_fields(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &Document,
    prefix: &str,
    group_obj: &mut serde_json::Map<String, Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in fields {
        let full_name = prefixed_name(prefix, &field.name);
        let locale = locale::resolve_join_locale(field, locale_ctx);
        let fallback_locale = locale::resolve_join_fallback_locale(field, locale_ctx);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    let val = hydrate_relationship(
                        conn,
                        slug,
                        &full_name,
                        &doc.id,
                        rc,
                        locale.as_deref(),
                        fallback_locale.as_deref(),
                    )?;
                    group_obj.insert(field.name.clone(), val);
                }
            }
            FieldType::Array => {
                let val = hydrate_array(
                    conn,
                    slug,
                    &full_name,
                    &doc.id,
                    &field.fields,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;
                group_obj.insert(field.name.clone(), val);
            }
            FieldType::Blocks => {
                let val = hydrate_blocks(
                    conn,
                    slug,
                    &full_name,
                    &doc.id,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;
                group_obj.insert(field.name.clone(), val);
            }
            FieldType::Group => {
                if let Some(Value::Object(sub_obj)) = group_obj.get_mut(&field.name) {
                    hydrate_group_join_fields(
                        conn,
                        slug,
                        &field.fields,
                        doc,
                        &full_name,
                        sub_obj,
                        locale_ctx,
                    )?;
                } else {
                    let mut sub_obj = serde_json::Map::new();

                    hydrate_group_join_fields(
                        conn,
                        slug,
                        &field.fields,
                        doc,
                        &full_name,
                        &mut sub_obj,
                        locale_ctx,
                    )?;

                    if !sub_obj.is_empty() {
                        group_obj.insert(field.name.clone(), Value::Object(sub_obj));
                    }
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                hydrate_group_join_fields(
                    conn,
                    slug,
                    &field.fields,
                    doc,
                    prefix,
                    group_obj,
                    locale_ctx,
                )?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hydrate_group_join_fields(
                        conn,
                        slug,
                        &tab.fields,
                        doc,
                        prefix,
                        group_obj,
                        locale_ctx,
                    )?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Batched twin of [`hydrate_group_join_fields`]: hydrate the join-shaped
/// fields nested inside a Group across MANY docs at once. `group_objs` is a
/// per-doc object slice parallel to `docs`; results land under bare field
/// names inside each doc's object, exactly like the per-doc walk. Join-table
/// lookups use the `__`-prefixed table names; each (field, table) pair costs
/// one batched query (plus one fallback query when needed) regardless of the
/// number of docs.
fn hydrate_group_join_fields_batch(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    docs: &mut [Document],
    prefix: &str,
    group_objs: &mut [serde_json::Map<String, Value>],
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let parent_ids: Vec<String> = docs.iter().map(|d| d.id.to_string()).collect();
    let parent_refs: Vec<&str> = parent_ids.iter().map(String::as_str).collect();

    for field in fields {
        let full_name = prefixed_name(prefix, &field.name);
        let locale = locale::resolve_join_locale(field, locale_ctx);
        let fallback_locale = locale::resolve_join_fallback_locale(field, locale_ctx);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    let mut grouped = fetch_related_json_grouped(
                        conn,
                        slug,
                        &full_name,
                        rc,
                        &parent_refs,
                        locale.as_deref(),
                        fallback_locale.as_deref(),
                    )?;
                    distribute_into_group_objs(docs, group_objs, &field.name, &mut grouped);
                }
            }
            FieldType::Array => {
                let mut grouped = fetch_array_rows_grouped(
                    conn,
                    slug,
                    &full_name,
                    &field.fields,
                    &parent_refs,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;
                distribute_into_group_objs(docs, group_objs, &field.name, &mut grouped);
            }
            FieldType::Blocks => {
                let mut grouped = fetch_block_rows_grouped(
                    conn,
                    slug,
                    &full_name,
                    &parent_refs,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;
                distribute_into_group_objs(docs, group_objs, &field.name, &mut grouped);
            }
            FieldType::Group => {
                // Take (or create) each doc's nested sub-object, recurse the
                // batched walk into it, and put back the non-empty results.
                let mut sub_objs: Vec<serde_json::Map<String, Value>> = group_objs
                    .iter_mut()
                    .map(|obj| match obj.remove(&field.name) {
                        Some(Value::Object(m)) => m,
                        _ => serde_json::Map::new(),
                    })
                    .collect();

                hydrate_group_join_fields_batch(
                    conn,
                    slug,
                    &field.fields,
                    docs,
                    &full_name,
                    &mut sub_objs,
                    locale_ctx,
                )?;

                for (obj, sub) in group_objs.iter_mut().zip(sub_objs) {
                    if !sub.is_empty() {
                        obj.insert(field.name.clone(), Value::Object(sub));
                    }
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                hydrate_group_join_fields_batch(
                    conn,
                    slug,
                    &field.fields,
                    docs,
                    prefix,
                    group_objs,
                    locale_ctx,
                )?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hydrate_group_join_fields_batch(
                        conn,
                        slug,
                        &tab.fields,
                        docs,
                        prefix,
                        group_objs,
                        locale_ctx,
                    )?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Assign each doc's bucket (or `[]`) from `grouped` into its group object
/// under `key` — the group-walk twin of the doc-level distribute loops.
fn distribute_into_group_objs(
    docs: &[Document],
    group_objs: &mut [serde_json::Map<String, Value>],
    key: &str,
    grouped: &mut HashMap<String, Vec<Value>>,
) {
    for (doc, obj) in docs.iter().zip(group_objs.iter_mut()) {
        let rows = grouped.remove(doc.id.as_ref()).unwrap_or_default();
        obj.insert(key.to_string(), Value::Array(rows));
    }
}

/// Batched hydrate for a single has-many relationship field across
/// every doc in `docs`. Issues one `IN (…)` SELECT instead of one
/// SELECT per doc, then distributes results back. Preserves the
/// locale-fallback semantics of the per-doc path by running a second
/// batched query against the fallback locale for only the parents
/// that came back empty.
fn hydrate_relationship_batch(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    rc: &RelationshipConfig,
    docs: &mut [Document],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<()> {
    let parent_ids: Vec<&str> = docs.iter().map(|d| d.id.as_ref()).collect();

    if rc.is_polymorphic() {
        let mut grouped =
            find_polymorphic_related_batch(conn, slug, field_name, &parent_ids, locale_ref)?;
        if let Some(fb) = fallback_ref {
            let missing: Vec<&str> = parent_ids
                .iter()
                .filter(|id| !grouped.contains_key(**id))
                .copied()
                .collect();
            if !missing.is_empty() {
                let fb_grouped =
                    find_polymorphic_related_batch(conn, slug, field_name, &missing, Some(fb))?;
                grouped.extend(fb_grouped);
            }
        }
        for doc in docs.iter_mut() {
            let items = grouped.remove(doc.id.as_ref()).unwrap_or_default();
            let json_items: Vec<Value> = items
                .into_iter()
                .map(|(col, id)| Value::String(poly_ref::format(&col, &id)))
                .collect();
            doc.fields
                .insert(field_name.to_string(), Value::Array(json_items));
        }
    } else {
        let mut grouped: HashMap<String, Vec<String>> =
            find_related_ids_batch(conn, slug, field_name, &parent_ids, locale_ref)?;
        if let Some(fb) = fallback_ref {
            let missing: Vec<&str> = parent_ids
                .iter()
                .filter(|id| !grouped.contains_key(**id))
                .copied()
                .collect();
            if !missing.is_empty() {
                let fb_grouped =
                    find_related_ids_batch(conn, slug, field_name, &missing, Some(fb))?;
                grouped.extend(fb_grouped);
            }
        }
        for doc in docs.iter_mut() {
            let ids = grouped.remove(doc.id.as_ref()).unwrap_or_default();
            let json_ids: Vec<Value> = ids.into_iter().map(Value::String).collect();
            doc.fields
                .insert(field_name.to_string(), Value::Array(json_ids));
        }
    }

    Ok(())
}

/// Run a batched grouped fetch with per-parent locale fallback: parents
/// absent from the primary-locale result are re-queried against the fallback
/// locale in ONE second batched query. Mirrors the per-doc "empty ⇒ retry
/// with fallback" semantics exactly, per parent.
fn fetch_grouped_with_fallback<T>(
    parent_ids: &[&str],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
    mut fetch: impl FnMut(&[&str], Option<&str>) -> Result<HashMap<String, Vec<T>>>,
) -> Result<HashMap<String, Vec<T>>> {
    let mut grouped = fetch(parent_ids, locale_ref)?;

    if let Some(fb) = fallback_ref {
        let missing: Vec<&str> = parent_ids
            .iter()
            .filter(|id| !grouped.contains_key(**id))
            .copied()
            .collect();
        if !missing.is_empty() {
            grouped.extend(fetch(&missing, Some(fb))?);
        }
    }

    Ok(grouped)
}

/// Grouped array rows for many parents, with locale fallback. `field_name`
/// is the join-table field name (`__`-prefixed inside groups).
fn fetch_array_rows_grouped(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    sub_fields: &[FieldDefinition],
    parent_ids: &[&str],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<HashMap<String, Vec<Value>>> {
    fetch_grouped_with_fallback(parent_ids, locale_ref, fallback_ref, |ids, loc| {
        find_array_rows_batch(conn, slug, field_name, ids, sub_fields, loc)
    })
}

/// Grouped block rows for many parents, with locale fallback.
fn fetch_block_rows_grouped(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    parent_ids: &[&str],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<HashMap<String, Vec<Value>>> {
    fetch_grouped_with_fallback(parent_ids, locale_ref, fallback_ref, |ids, loc| {
        find_block_rows_batch(conn, slug, field_name, ids, loc)
    })
}

/// Grouped has-many relationship values (JSON-ready) for many parents, with
/// locale fallback. Polymorphic refs render as `collection:id` strings.
fn fetch_related_json_grouped(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    rc: &RelationshipConfig,
    parent_ids: &[&str],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<HashMap<String, Vec<Value>>> {
    if rc.is_polymorphic() {
        fetch_grouped_with_fallback(parent_ids, locale_ref, fallback_ref, |ids, loc| {
            let grouped = find_polymorphic_related_batch(conn, slug, field_name, ids, loc)?;
            Ok(grouped
                .into_iter()
                .map(|(parent, items)| {
                    let json: Vec<Value> = items
                        .into_iter()
                        .map(|(col, id)| Value::String(poly_ref::format(&col, &id)))
                        .collect();
                    (parent, json)
                })
                .collect())
        })
    } else {
        fetch_grouped_with_fallback(parent_ids, locale_ref, fallback_ref, |ids, loc| {
            let grouped = find_related_ids_batch(conn, slug, field_name, ids, loc)?;
            Ok(grouped
                .into_iter()
                .map(|(parent, ids)| (parent, ids.into_iter().map(Value::String).collect()))
                .collect())
        })
    }
}

/// Batched hydrate for one array field across every doc in `docs` — one
/// `IN (…)` SELECT instead of one per doc (fallback semantics preserved
/// per parent).
fn hydrate_array_batch(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    sub_fields: &[FieldDefinition],
    docs: &mut [Document],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<()> {
    let parent_ids: Vec<&str> = docs.iter().map(|d| d.id.as_ref()).collect();
    let mut grouped = fetch_array_rows_grouped(
        conn,
        slug,
        field_name,
        sub_fields,
        &parent_ids,
        locale_ref,
        fallback_ref,
    )?;

    for doc in docs.iter_mut() {
        let rows = grouped.remove(doc.id.as_ref()).unwrap_or_default();
        doc.fields
            .insert(field_name.to_string(), Value::Array(rows));
    }

    Ok(())
}

/// Batched hydrate for one blocks field across every doc in `docs` — same
/// shape as [`hydrate_array_batch`].
fn hydrate_blocks_batch(
    conn: &dyn DbConnection,
    slug: &str,
    field_name: &str,
    docs: &mut [Document],
    locale_ref: Option<&str>,
    fallback_ref: Option<&str>,
) -> Result<()> {
    let parent_ids: Vec<&str> = docs.iter().map(|d| d.id.as_ref()).collect();
    let mut grouped = fetch_block_rows_grouped(
        conn,
        slug,
        field_name,
        &parent_ids,
        locale_ref,
        fallback_ref,
    )?;

    for doc in docs.iter_mut() {
        let rows = grouped.remove(doc.id.as_ref()).unwrap_or_default();
        doc.fields
            .insert(field_name.to_string(), Value::Array(rows));
    }

    Ok(())
}

/// Batched hydrate over a list of documents.
///
/// EVERY join-shaped field issues one `WHERE parent_id IN (…)` SELECT per
/// field instead of one per (doc, field) pair: top-level has-many
/// relationships, arrays, blocks, and — via the recursive
/// [`hydrate_group_join_fields_batch`] walk — join-shaped fields nested in
/// groups at any depth (`Row`/`Collapsible`/`Tabs` recurse structurally).
/// A `find` returning 10 docs with 3 join-shaped fields previously did 30
/// SELECTs; this batches it to 3 (plus at most one fallback query per field
/// for the parents whose primary-locale read came back empty).
///
/// # Errors
///
/// Returns a backend error if any of the join-table queries fails.
pub fn hydrate_documents(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    docs: &mut [Document],
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    if docs.is_empty() {
        return Ok(());
    }

    for doc in docs.iter_mut() {
        parse_has_many_scalar_columns(fields, doc);
    }

    for field in fields {
        if let Some(sel) = select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }

        let locale = locale::resolve_join_locale(field, locale_ctx);
        let fallback_locale = locale::resolve_join_fallback_locale(field, locale_ctx);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    hydrate_relationship_batch(
                        conn,
                        slug,
                        &field.name,
                        rc,
                        docs,
                        locale.as_deref(),
                        fallback_locale.as_deref(),
                    )?;
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                hydrate_documents(conn, slug, &field.fields, docs, select, locale_ctx)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hydrate_documents(conn, slug, &tab.fields, docs, select, locale_ctx)?;
                }
            }
            FieldType::Array => {
                hydrate_array_batch(
                    conn,
                    slug,
                    &field.name,
                    &field.fields,
                    docs,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;
            }
            FieldType::Blocks => {
                hydrate_blocks_batch(
                    conn,
                    slug,
                    &field.name,
                    docs,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;
            }
            FieldType::Group => {
                // Scalar group reconstruction is in-memory per doc; the
                // join-shaped sub-fields are batched across all docs by the
                // recursive group walk below.
                let mut group_objs: Vec<serde_json::Map<String, Value>> = docs
                    .iter_mut()
                    .map(|doc| {
                        let mut obj = serde_json::Map::new();
                        reconstruct_group_fields(&field.fields, &field.name, doc, &mut obj);
                        obj
                    })
                    .collect();

                hydrate_group_join_fields_batch(
                    conn,
                    slug,
                    &field.fields,
                    docs,
                    &field.name,
                    &mut group_objs,
                    locale_ctx,
                )?;

                for (doc, obj) in docs.iter_mut().zip(group_objs) {
                    if !obj.is_empty() {
                        doc.fields.insert(field.name.clone(), Value::Object(obj));
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Hydrate a document with join table data (has-many relationships and arrays).
/// Populates `doc.fields` with JSON arrays for each join-table field.
/// If `select` is provided, skip hydrating fields not in the select list.
/// When `locale_ctx` is provided, localized join fields are filtered by locale.
///
/// # Errors
///
/// Returns a backend error if any of the join-table queries fails.
pub fn hydrate_document(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &mut Document,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    parse_has_many_scalar_columns(fields, doc);

    for field in fields {
        if let Some(sel) = select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }

        let locale = locale::resolve_join_locale(field, locale_ctx);
        let fallback_locale = locale::resolve_join_fallback_locale(field, locale_ctx);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    let val = hydrate_relationship(
                        conn,
                        slug,
                        &field.name,
                        &doc.id,
                        rc,
                        locale.as_deref(),
                        fallback_locale.as_deref(),
                    )?;

                    doc.fields.insert(field.name.clone(), val);
                }
            }
            FieldType::Array => {
                let val = hydrate_array(
                    conn,
                    slug,
                    &field.name,
                    &doc.id,
                    &field.fields,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;

                doc.fields.insert(field.name.clone(), val);
            }
            FieldType::Blocks => {
                let val = hydrate_blocks(
                    conn,
                    slug,
                    &field.name,
                    &doc.id,
                    locale.as_deref(),
                    fallback_locale.as_deref(),
                )?;

                doc.fields.insert(field.name.clone(), val);
            }
            FieldType::Group => {
                let mut group_obj = serde_json::Map::new();

                reconstruct_group_fields(&field.fields, &field.name, doc, &mut group_obj);

                hydrate_group_join_fields(
                    conn,
                    slug,
                    &field.fields,
                    doc,
                    &field.name,
                    &mut group_obj,
                    locale_ctx,
                )?;

                if !group_obj.is_empty() {
                    doc.fields
                        .insert(field.name.clone(), Value::Object(group_obj));
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                hydrate_document(conn, slug, &field.fields, doc, select, locale_ctx)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hydrate_document(conn, slug, &tab.fields, doc, select, locale_ctx)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;
    use serde_json::{Value, json};

    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::FieldType;
    use crate::db::query::test_helpers::CountingConn;
    use crate::db::query::{LocaleContext, LocaleMode};

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    /// posts with a top-level array (`items`), a blocks field (`content`),
    /// and a group (`meta`) containing a nested array (`links`).
    fn make_fields() -> Vec<FieldDefinition> {
        vec![
            text_field("title"),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label")])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks).build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    text_field("note"),
                    FieldDefinition::builder("links", FieldType::Array)
                        .fields(vec![text_field("url")])
                        .build(),
                ])
                .build(),
        ]
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, meta__note TEXT);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT
             );
             CREATE TABLE posts_content (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER,
                 _block_type TEXT, data TEXT
             );
             CREATE TABLE posts_meta__links (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, url TEXT
             );
             INSERT INTO posts_items VALUES
                 ('i1', 'p1', 0, 'One'), ('i2', 'p1', 1, 'Two'), ('i3', 'p2', 0, 'Three');
             INSERT INTO posts_content VALUES
                 ('b1', 'p1', 0, 'hero', '{\"heading\":\"H1\"}'),
                 ('b2', 'p2', 0, 'cta', '{\"label\":\"Go\"}');
             INSERT INTO posts_meta__links VALUES
                 ('l1', 'p1', 0, 'https://a'), ('l2', 'p2', 0, 'https://b');",
        )
        .unwrap();
        conn
    }

    fn make_docs(ids: &[&str]) -> Vec<Document> {
        ids.iter()
            .map(|id| {
                let mut d = Document::new((*id).to_string());
                d.fields.insert("title".to_string(), json!("t"));
                d.fields.insert("meta__note".to_string(), json!("n"));
                d
            })
            .collect()
    }

    fn arr_of(doc: &Document, key: &str) -> Vec<Value> {
        doc.fields
            .get(key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_else(|| panic!("{key} missing on {}", doc.id))
    }

    /// Correctness across the batch: each parent gets exactly its own array
    /// rows, block rows, and group-nested array rows; a parent with no rows
    /// gets `[]`, not a missing key.
    #[test]
    fn batch_hydrate_buckets_arrays_blocks_and_group_arrays_per_parent() {
        let conn = setup_db();
        let fields = make_fields();
        let mut docs = make_docs(&["p1", "p2", "p3"]);

        hydrate_documents(&conn, "posts", &fields, &mut docs, None, None).unwrap();

        let p1_items = arr_of(&docs[0], "items");
        assert_eq!(p1_items.len(), 2);
        assert_eq!(p1_items[0]["label"], "One");
        assert_eq!(p1_items[1]["label"], "Two");
        assert_eq!(arr_of(&docs[1], "items")[0]["label"], "Three");
        assert!(
            arr_of(&docs[2], "items").is_empty(),
            "no-row parent gets []"
        );

        assert_eq!(arr_of(&docs[0], "content")[0]["heading"], "H1");
        assert_eq!(arr_of(&docs[1], "content")[0]["label"], "Go");
        assert!(arr_of(&docs[2], "content").is_empty());

        let p1_meta = docs[0].fields.get("meta").expect("meta group");
        assert_eq!(p1_meta["note"], "n");
        assert_eq!(p1_meta["links"][0]["url"], "https://a");
        let p2_meta = docs[1].fields.get("meta").expect("meta group");
        assert_eq!(p2_meta["links"][0]["url"], "https://b");
    }

    /// Regression (B2): the number of read queries must be CONSTANT in the
    /// number of documents — one per join-shaped field (array, blocks,
    /// group-nested array), not one per (doc, field). Before batching,
    /// hydrating N docs cost N queries per array/blocks/group field.
    #[test]
    fn batch_hydrate_query_count_is_independent_of_doc_count() {
        let conn = setup_db();
        let fields = make_fields();

        let counting = CountingConn::new(&conn);
        let mut one = make_docs(&["p1"]);
        hydrate_documents(&counting, "posts", &fields, &mut one, None, None).unwrap();
        let reads_for_one = counting.reads();

        let counting = CountingConn::new(&conn);
        let mut three = make_docs(&["p1", "p2", "p3"]);
        hydrate_documents(&counting, "posts", &fields, &mut three, None, None).unwrap();
        let reads_for_three = counting.reads();

        assert_eq!(
            reads_for_one, reads_for_three,
            "hydrate query count must not scale with document count"
        );
        assert_eq!(
            reads_for_three, 3,
            "one query per join-shaped field (items, content, meta.links)"
        );
    }

    /// Per-parent locale fallback across the batch: the parent with rows in
    /// the requested locale gets them; the parent with rows only in the
    /// default locale falls back — independently, in the same batch.
    #[test]
    fn batch_hydrate_array_locale_fallback_is_per_parent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER,
                 _locale TEXT, label TEXT
             );
             INSERT INTO posts_items VALUES
                 ('i1', 'p1', 0, 'de', 'Deutsch'),
                 ('i2', 'p2', 0, 'en', 'English');",
        )
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .localized(true)
                .fields(vec![text_field("label")])
                .build(),
        ];

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };

        let mut docs: Vec<Document> = ["p1", "p2"]
            .iter()
            .map(|id| Document::new((*id).to_string()))
            .collect();

        hydrate_documents(&conn, "posts", &fields, &mut docs, None, Some(&locale_ctx)).unwrap();

        assert_eq!(
            arr_of(&docs[0], "items")[0]["label"],
            "Deutsch",
            "p1 has de rows and must get them"
        );
        assert_eq!(
            arr_of(&docs[1], "items")[0]["label"],
            "English",
            "p2 has no de rows and must fall back to the default locale"
        );
    }
}
