//! Document hydration — populates join-table fields (arrays, blocks, relationships)
//! into documents after the main row query.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use super::{
    super::{
        arrays::find_array_rows,
        blocks::find_block_rows,
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
        query::helpers::{parse_has_many_scalar, prefixed_name, walk_leaf_fields},
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
            .map(|(col, id)| Value::String(format!("{col}/{id}")))
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
                .map(|(col, id)| Value::String(format!("{col}/{id}")))
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

/// Batched hydrate over a list of documents.
///
/// For top-level has-many relationship fields (the common case on
/// list endpoints — `posts.tags`, `posts.categories`, …), issues one
/// `WHERE parent_id IN (…)` SELECT per field instead of one per
/// (doc, field) pair. A `find` returning 10 docs with 3 relationship
/// fields previously did 30 SELECTs; this batches it to 3.
///
/// Non-batched field shapes (`Array`, `Blocks`, and relationship
/// fields nested inside `Group`/`Tabs`/`Row`/`Collapsible`) fall
/// through to the per-doc [`hydrate_document`] path. Arrays and
/// blocks could be batched with the same pattern; relationship
/// fields inside groups would need a recursive group-aware
/// batched walk. Neither is implemented yet — `hydrate_documents`
/// is purposefully scoped to the field shape that dominated the
/// `find @ 50` profile.
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
            FieldType::Array | FieldType::Blocks | FieldType::Group => {
                // Not batched yet — delegate to the per-doc path for
                // these shapes. We pass a single-element field slice
                // so only this field is processed per doc.
                let single = std::slice::from_ref(field);
                for doc in docs.iter_mut() {
                    hydrate_document(conn, slug, single, doc, select, locale_ctx)?;
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
