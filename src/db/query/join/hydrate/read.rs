//! Document hydration — populates join-table fields (arrays, blocks, relationships)
//! into documents after the main row query.

use anyhow::Result;
use serde_json::Value;

use super::{
    super::{
        arrays::find_array_rows,
        blocks::find_block_rows,
        relationships::{find_polymorphic_related, find_related_ids},
    },
    group::reconstruct_group_fields,
    locale,
};
use crate::{
    core::{Document, FieldDefinition, FieldType},
    db::{DbConnection, LocaleContext},
};

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
        let full_name = format!("{}__{}", prefix, field.name);
        let locale = locale::resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        let fallback_locale = locale::resolve_join_fallback_locale(field, locale_ctx);
        let fallback_ref = fallback_locale.as_deref();
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    if rc.is_polymorphic() {
                        let mut items =
                            find_polymorphic_related(conn, slug, &full_name, &doc.id, locale_ref)?;
                        if items.is_empty() && fallback_ref.is_some() {
                            items = find_polymorphic_related(
                                conn,
                                slug,
                                &full_name,
                                &doc.id,
                                fallback_ref,
                            )?;
                        }
                        let json_items: Vec<Value> = items
                            .into_iter()
                            .map(|(col, id)| Value::String(format!("{}/{}", col, id)))
                            .collect();
                        group_obj.insert(field.name.clone(), Value::Array(json_items));
                    } else {
                        let mut ids =
                            find_related_ids(conn, slug, &full_name, &doc.id, locale_ref)?;
                        if ids.is_empty() && fallback_ref.is_some() {
                            ids = find_related_ids(conn, slug, &full_name, &doc.id, fallback_ref)?;
                        }
                        let json_ids: Vec<Value> = ids.into_iter().map(Value::String).collect();
                        group_obj.insert(field.name.clone(), Value::Array(json_ids));
                    }
                }
            }
            FieldType::Array => {
                let mut rows =
                    find_array_rows(conn, slug, &full_name, &doc.id, &field.fields, locale_ref)?;
                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_array_rows(
                        conn,
                        slug,
                        &full_name,
                        &doc.id,
                        &field.fields,
                        fallback_ref,
                    )?;
                }
                group_obj.insert(field.name.clone(), Value::Array(rows));
            }
            FieldType::Blocks => {
                let mut rows = find_block_rows(conn, slug, &full_name, &doc.id, locale_ref)?;
                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_block_rows(conn, slug, &full_name, &doc.id, fallback_ref)?;
                }
                group_obj.insert(field.name.clone(), Value::Array(rows));
            }
            FieldType::Group => {
                // Nested group: recurse with extended prefix
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

/// Hydrate a document with join table data (has-many relationships and arrays).
/// Populates `doc.fields` with JSON arrays for each join-table field.
/// If `select` is provided, skip hydrating fields not in the select list.
/// When `locale_ctx` is provided, localized join fields are filtered by locale.
pub fn hydrate_document(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &mut Document,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in fields {
        // Skip hydrating fields not in the select list
        if let Some(sel) = select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }
        let locale = locale::resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        let fallback_locale = locale::resolve_join_fallback_locale(field, locale_ctx);
        let fallback_ref = fallback_locale.as_deref();
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
                    if rc.is_polymorphic() {
                        let mut items =
                            find_polymorphic_related(conn, slug, &field.name, &doc.id, locale_ref)?;

                        if items.is_empty() && fallback_ref.is_some() {
                            items = find_polymorphic_related(
                                conn,
                                slug,
                                &field.name,
                                &doc.id,
                                fallback_ref,
                            )?;
                        }
                        let json_items: Vec<Value> = items
                            .into_iter()
                            .map(|(col, id)| Value::String(format!("{}/{}", col, id)))
                            .collect();
                        doc.fields
                            .insert(field.name.clone(), Value::Array(json_items));
                    } else {
                        let mut ids =
                            find_related_ids(conn, slug, &field.name, &doc.id, locale_ref)?;

                        if ids.is_empty() && fallback_ref.is_some() {
                            ids = find_related_ids(conn, slug, &field.name, &doc.id, fallback_ref)?;
                        }
                        let json_ids: Vec<Value> = ids.into_iter().map(Value::String).collect();
                        doc.fields
                            .insert(field.name.clone(), Value::Array(json_ids));
                    }
                }
            }
            FieldType::Array => {
                let mut rows =
                    find_array_rows(conn, slug, &field.name, &doc.id, &field.fields, locale_ref)?;

                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_array_rows(
                        conn,
                        slug,
                        &field.name,
                        &doc.id,
                        &field.fields,
                        fallback_ref,
                    )?;
                }
                doc.fields.insert(field.name.clone(), Value::Array(rows));
            }
            FieldType::Group => {
                // Reconstruct nested object from prefixed columns: seo__title → { seo: { title: val } }
                let mut group_obj = serde_json::Map::new();
                let prefix = &field.name;
                reconstruct_group_fields(&field.fields, prefix, doc, &mut group_obj);

                // Hydrate join-table types (Array, Blocks, Relationship) inside the Group
                hydrate_group_join_fields(
                    conn,
                    slug,
                    &field.fields,
                    doc,
                    prefix,
                    &mut group_obj,
                    locale_ctx,
                )?;

                if !group_obj.is_empty() {
                    doc.fields
                        .insert(field.name.clone(), Value::Object(group_obj));
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                // Sub-fields are top-level columns, but recurse for join-table types (blocks, arrays, relationships)
                hydrate_document(conn, slug, &field.fields, doc, select, locale_ctx)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hydrate_document(conn, slug, &tab.fields, doc, select, locale_ctx)?;
                }
            }
            FieldType::Blocks => {
                let mut rows = find_block_rows(conn, slug, &field.name, &doc.id, locale_ref)?;

                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_block_rows(conn, slug, &field.name, &doc.id, fallback_ref)?;
                }
                doc.fields.insert(field.name.clone(), Value::Array(rows));
            }
            _ => {}
        }
    }
    Ok(())
}
