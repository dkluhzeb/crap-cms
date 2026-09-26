//! Join field population — the reverse lookup a join lists, shared by the
//! single-document and the batch path and the admin edit form
//! ([`join_children`](super::join_children)), so every surface lists exactly
//! the same children.
//!
//! A join lists the target documents whose `on` field references the
//! document the join belongs to: at most the join's `limit` per document, in
//! the target's default order, only the ones the reader may see (the target's
//! `read` / `draft` views, applied in SQL), each shown as the reader's draft
//! view where that applies, and only those whose `on` value the reader may
//! read — a child's presence in the join IS that value.
//!
//! Field read access is a per-document verdict, so it cannot be folded into
//! the SQL. The lookup therefore reads each parent's children in windows —
//! the first `limit`, then ever larger windows further down the order — and
//! keeps reading for a parent until it lists `limit` children or its children
//! run out; the limit counts listed children, never ones dropped later.

use std::{collections::HashMap, ops::Range};

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{
        CollectionDefinition, Document, FieldDefinition, FieldType, JoinConfig,
        field::flatten_array_sub_fields,
    },
    db::{
        Filter, FilterClause, FilterOp, FindQuery, LocaleContext, ViewScope,
        query::{GroupLimit, GroupedFind, JoinReaders, find_grouped, hydrate_documents},
    },
};

use super::{
    PopulateCtx,
    helpers::{TargetCollection, TargetViews, resolve_target_views, visible_targets},
};

/// Children listed per parent id, each list in the target's default order.
type Listed = HashMap<String, Vec<Document>>;

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
/// each list in the target's default order and at most the join's limit long
/// (see the module docs). A child is listed under the parent its STORED `on`
/// value names — the value the lookup matched — even when the reader sees a
/// pending draft of it that names another.
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
) -> Result<Listed> {
    if parent_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let views = resolve_target_views(ctx, &join.collection, target_def)?;
    let lookup = JoinLookup {
        ctx,
        join,
        target_def,
        view_filters: view_filters(ctx, target_def, &views),
        views: &views,
    };

    let limit = i64::from(join.effective_limit());
    let mut listed = Listed::new();
    let mut pending = parent_ids;
    let mut rows = 0..limit;

    while !pending.is_empty() {
        let window = lookup.read_window(&pending, rows.clone())?;

        pending = window.settle(&mut listed, limit, &rows);
        rows = next_window(&rows);
    }

    Ok(listed)
}

/// The reader's view filters on the target — `read`, and `draft` when drafts
/// are requested — joined with AND into every window.
fn view_filters(
    ctx: &PopulateCtx<'_>,
    target_def: &CollectionDefinition,
    views: &TargetViews,
) -> Vec<FilterClause> {
    ViewScope::assemble(
        target_def.has_drafts(),
        Some(views.read.clone()),
        (!ctx.published_only).then(|| views.draft.clone()),
    )
    .into_filters()
}

/// The window after `rows`: the positions right below it, twice as many, so a
/// parent whose children the reader mostly may not list takes few reads.
fn next_window(rows: &Range<i64>) -> Range<i64> {
    let len = rows.end - rows.start;

    rows.end..rows.end.saturating_add(len.saturating_mul(2))
}

/// One join lookup: the join, its target, and the reader's views of it. Built
/// in one place with every field set.
struct JoinLookup<'a, 'c> {
    ctx: &'a PopulateCtx<'c>,
    join: &'a JoinConfig,
    target_def: &'a CollectionDefinition,
    views: &'a TargetViews,
    view_filters: Vec<FilterClause>,
}

impl JoinLookup<'_, '_> {
    /// The children at positions `rows` of each of `parents`' groups, as the
    /// reader sees them, with how many rows each group gave.
    fn read_window(&self, parents: &[String], rows: Range<i64>) -> Result<Window> {
        let mut found = self.find_window(parents, rows)?;
        let mut stored = stored_parents(&found, &self.join.on);
        let read = rows_per_parent(&stored);

        hydrate_documents(
            self.ctx.conn,
            &self.join.collection,
            &self.target_def.fields,
            &mut found,
            None,
            self.ctx.locale_ctx,
        )?;

        let target =
            TargetCollection::builder(&self.join.collection, self.target_def, self.views).build();
        let shown = visible_targets(self.ctx, &target, found)?;
        let readable = self.on_readable(&shown);

        let kept = shown
            .into_iter()
            .zip(readable)
            .filter(|(_, readable)| *readable)
            .filter_map(|(child, _)| Some((stored.remove(child.id.as_ref())?, child)))
            .collect();

        Ok(Window { read, kept })
    }

    /// The raw rows at positions `rows` of each of `parents`' groups.
    fn find_window(&self, parents: &[String], rows: Range<i64>) -> Result<Vec<Document>> {
        let mut filters = self.view_filters.clone();
        filters.push(FilterClause::Single(Filter {
            field: self.join.on.clone(),
            op: FilterOp::In(parents.to_vec()),
        }));

        let query = FindQuery::builder().filters(filters).build();
        let group = GroupLimit::window(&self.join.on, rows);
        let find = GroupedFind::builder(&self.join.collection, self.target_def, &query, group)
            .locale_ctx(self.ctx.locale_ctx)
            .build();

        find_grouped(self.ctx.conn, &find)
    }

    /// Whether each of `shown` keeps its `on` value through the reader's field
    /// read strip; every child when no access check is wired.
    fn on_readable(&self, shown: &[Document]) -> Vec<bool> {
        let Some(check) = self.ctx.join_access else {
            return vec![true; shown.len()];
        };

        let readers = JoinReaders::builder(self.join, self.target_def)
            .user(self.ctx.user)
            .locale(self.ctx.locale_ctx.map(LocaleContext::access_locale))
            .build();

        check.on_readable(&readers, shown)
    }
}

/// What one window read: the rows each parent's group gave, and the children
/// it lists, each with the parent its stored `on` value names, in order.
struct Window {
    read: HashMap<String, i64>,
    kept: Vec<(String, Document)>,
}

impl Window {
    /// List the kept children (each parent up to `limit`) and return the
    /// parents still short whose group may hold more: those the window at
    /// `rows` filled completely.
    ///
    /// A child already listed is skipped: each window is a separate read, and
    /// a child written above a group's earlier window in between shifts the
    /// rows down, so a later window can start with a child listed before.
    fn settle(self, listed: &mut Listed, limit: i64, rows: &Range<i64>) -> Vec<String> {
        let cap = usize::try_from(limit).unwrap_or(usize::MAX);

        for (parent, child) in self.kept {
            let children = listed.entry(parent).or_default();

            if children.len() < cap && !children.iter().any(|listed| listed.id == child.id) {
                children.push(child);
            }
        }

        let full = rows.end - rows.start;

        self.read
            .into_iter()
            .filter(|(_, count)| *count >= full)
            .filter(|(parent, _)| listed.get(parent).map_or(0, Vec::len) < cap)
            .map(|(parent, _)| parent)
            .collect()
    }
}

/// The parent each raw child's stored `on` value names, keyed by child id — the
/// value the lookup matched, captured before any draft overlay.
fn stored_parents(raws: &[Document], on: &str) -> HashMap<String, String> {
    raws.iter()
        .filter_map(|raw| Some((raw.id.to_string(), join_key_from_value(raw.fields.get(on))?)))
        .collect()
}

/// How many rows each parent's group gave a window.
fn rows_per_parent(stored: &HashMap<String, String>) -> HashMap<String, i64> {
    let mut counts = HashMap::new();

    for parent in stored.values() {
        *counts.entry(parent.clone()).or_insert(0) += 1;
    }

    counts
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

    fn child(id: &str) -> Document {
        Document::new(id.to_string())
    }

    /// Regression: a child written above a group's first window between two
    /// window reads shifted the rows down, so the next window began with a
    /// child already listed and the join listed it twice. A child already
    /// listed is skipped, and the parent stays pending while it is short.
    #[test]
    fn a_child_a_later_window_reads_again_is_listed_once() {
        let mut listed = Listed::new();
        listed.insert("a1".to_string(), vec![child("p1"), child("p2")]);

        let window = Window {
            read: HashMap::from([("a1".to_string(), 4)]),
            kept: vec![
                ("a1".to_string(), child("p2")),
                ("a1".to_string(), child("p3")),
            ],
        };

        let pending = window.settle(&mut listed, 4, &(2..6));

        let ids: Vec<&str> = listed["a1"].iter().map(|c| c.id.as_ref()).collect();
        assert_eq!(ids, vec!["p1", "p2", "p3"]);
        assert_eq!(pending, vec!["a1".to_string()]);
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
