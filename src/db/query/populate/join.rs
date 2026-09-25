//! Join field population — the reverse lookup a join lists, shared by the
//! single-document and the batch path so both list exactly the same children.
//!
//! A join lists the target documents whose `on` field references the
//! document the join belongs to: at most the join's `limit` per document, in
//! the target's default order, only the ones the reader may see (the target's
//! `read` / `draft` views, applied in SQL so the limit counts visible
//! children), each shown as the reader's draft view where that applies.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{
        CollectionDefinition, Document, FieldDefinition, FieldType, JoinConfig,
        field::flatten_array_sub_fields,
    },
    db::{
        Filter, FilterClause, FilterOp, FindQuery, ViewScope,
        query::{GroupLimit, GroupedFind, find_grouped, hydrate_documents},
    },
};

use super::{
    PopulateCtx,
    helpers::{TargetCollection, resolve_target_views, visible_targets},
};

/// The join fields a document populates itself: top-level ones and those in a
/// row / collapsible / tabs wrapper (transparent layout), filtered by the
/// read's `select`. A join in a group is populated by the container walker.
pub(super) fn document_join_fields<'a>(
    fields: &'a [FieldDefinition],
    select: Option<&[String]>,
) -> Vec<&'a FieldDefinition> {
    flatten_array_sub_fields(fields)
        .into_iter()
        .filter(|field| field.field_type == FieldType::Join && field.join.is_some())
        .filter(|field| select.is_none_or(|sel| sel.iter().any(|s| s == &field.name)))
        .collect()
}

/// The children `join` lists for each of `parent_ids`, keyed by parent id —
/// each list in the target's default order and at most the join's limit long.
///
/// # Errors
///
/// Propagates an access-hook or backend error: a failed lookup never reads as
/// an empty join.
pub(super) fn fetch_join_children(
    ctx: &PopulateCtx<'_>,
    join: &JoinConfig,
    target_def: &CollectionDefinition,
    parent_ids: Vec<String>,
) -> Result<HashMap<String, Vec<Document>>> {
    if parent_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let views = resolve_target_views(ctx, &join.collection, target_def)?;

    let mut filters = ViewScope::assemble(
        target_def.has_drafts(),
        Some(views.read.clone()),
        (!ctx.published_only).then(|| views.draft.clone()),
    )
    .into_filters();

    filters.push(FilterClause::Single(Filter {
        field: join.on.clone(),
        op: FilterOp::In(parent_ids),
    }));

    let query = FindQuery::builder().filters(filters).build();
    let limit = GroupLimit::new(&join.on, i64::from(join.effective_limit()));
    let find = GroupedFind::builder(&join.collection, target_def, &query, limit)
        .locale_ctx(ctx.locale_ctx)
        .build();

    let mut children = find_grouped(ctx.conn, &find)?;

    hydrate_documents(
        ctx.conn,
        &join.collection,
        &target_def.fields,
        &mut children,
        None,
        ctx.locale_ctx,
    )?;

    let target = TargetCollection::builder(&join.collection, target_def, &views).build();
    let children = visible_targets(ctx, &target, children)?;

    Ok(bucket_by_parent(children, &join.on))
}

/// Group join children by the parent their `on` value names, keeping order.
fn bucket_by_parent(children: Vec<Document>, on: &str) -> HashMap<String, Vec<Document>> {
    let mut buckets: HashMap<String, Vec<Document>> = HashMap::new();

    for child in children {
        let Some(parent) = join_key_from_value(child.fields.get(on)) else {
            continue;
        };

        buckets.entry(parent).or_default().push(child);
    }

    buckets
}

/// Coerce a doc-field `Value` to the string form used as a join-key bucket
/// label. Strings, numbers, and bools all become valid scalar keys; missing
/// values, arrays, and objects are non-scalar and cannot identify a single
/// parent row, so they are dropped.
fn join_key_from_value(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::JoinConfig;

    #[test]
    fn string_passes_through() {
        assert_eq!(
            join_key_from_value(Some(&json!("abc"))),
            Some("abc".to_string())
        );
    }

    #[test]
    fn integer_stringifies_without_quotes() {
        assert_eq!(
            join_key_from_value(Some(&json!(42))),
            Some("42".to_string())
        );
    }

    #[test]
    fn bool_stringifies() {
        assert_eq!(
            join_key_from_value(Some(&json!(true))),
            Some("true".to_string())
        );
    }

    /// Arrays and objects are not scalar join keys.
    #[test]
    fn array_and_object_are_rejected() {
        assert_eq!(join_key_from_value(Some(&json!([1, 2]))), None);
        assert_eq!(join_key_from_value(Some(&json!({"k": "v"}))), None);
    }

    #[test]
    fn null_and_missing_are_rejected() {
        assert_eq!(join_key_from_value(Some(&json!(null))), None);
        assert_eq!(join_key_from_value(None), None);
    }

    /// Regression: a join inside a row / collapsible / tabs wrapper was never
    /// populated — the document-level pass only looked at top-level fields.
    #[test]
    fn document_join_fields_see_through_layout_wrappers() {
        let join = |name: &str| {
            FieldDefinition::builder(name, FieldType::Join)
                .join(JoinConfig::new("posts", "author"))
                .build()
        };

        let row = FieldDefinition::builder("row", FieldType::Row)
            .fields(vec![join("in_row")])
            .build();
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![join("in_group")])
            .build();
        let fields = vec![join("top"), row, group];

        let names: Vec<&str> = document_join_fields(&fields, None)
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["top", "in_row"]);

        let selected = vec!["in_row".to_string()];
        let names: Vec<&str> = document_join_fields(&fields, Some(&selected))
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, vec!["in_row"]);
    }
}
