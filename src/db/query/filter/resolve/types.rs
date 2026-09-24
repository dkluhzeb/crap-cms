//! Resolved filter shape returned to the WHERE-clause builder.

use crate::{core::FieldType, db::query::filter::elements::ListLeaf};

/// A filter resolved to its SQL representation.
#[derive(Debug)]
pub(in crate::db::query::filter) enum ResolvedFilter {
    /// Direct column on the parent table, as the ready-to-embed SQL
    /// expression a read takes its value from — a quoted column, or the
    /// fallback `COALESCE` of a localized one, exactly as the SELECT emits it.
    ///
    /// `field_type` is the leaf field's type, used to cast filter operand
    /// values when binding. `None` when the type cannot be determined —
    /// binding falls back to `DbValue::Text`.
    ///
    /// `list` is set for a scalar has-many column (a JSON array): the filter
    /// then quantifies over its elements, and `expr` is qualified by the parent
    /// table so the element expansion's own columns cannot shadow it.
    Column {
        expr: String,
        field_type: Option<FieldType>,
        list: Option<ListLeaf>,
    },
    /// EXISTS subquery against a join table.
    Subquery {
        join_table: String,
        parent_table: String,
        condition: SubqueryCondition,
        /// The locale the join table's rows are matched in when they carry a
        /// `_locale` column — the locale hydration reads them in, with its
        /// fallback. `None` when the field is not localized or localization
        /// is off.
        rows_locale: Option<RowsLocale>,
    },
}

/// The locale a localized join field's rows are filtered in: `locale`, or
/// `fallback` for a document holding no row in `locale` — the rows the read
/// shows for that document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::db::query::filter) struct RowsLocale {
    pub(in crate::db::query::filter) locale: String,
    pub(in crate::db::query::filter) fallback: Option<String>,
}

impl RowsLocale {
    pub(in crate::db::query::filter) fn new(locale: &str, fallback: Option<&str>) -> Self {
        Self {
            locale: locale.to_string(),
            fallback: fallback.map(str::to_string),
        }
    }
}

/// How to access the filtered value within a subquery.
#[derive(Debug)]
pub(in crate::db::query::filter) enum SubqueryCondition {
    /// Direct column on an array or blocks join table (an array sub-field, or
    /// either row's own `id`).
    ///
    /// `field_type` drives operand casting; `None` means fall back to Text.
    /// `list` is set for a sub-field holding a list per row — a scalar has-many
    /// list or a has-many reference's id list — whose filter quantifies over
    /// the elements.
    Column {
        col: String,
        field_type: Option<FieldType>,
        list: Option<ListLeaf>,
    },
    /// The `related_id` column of a has-many relationship/upload junction —
    /// one row per referenced id, so the filter quantifies over the rows the
    /// way a scalar has-many filter quantifies over its elements.
    RelatedId,
    /// `_block_type` column on the join table. Always text.
    BlockType,
    /// `json_extract` on a row's JSON — a block row's `data`, an array row's
    /// group / nested array / nested blocks column — possibly with `json_each`
    /// joins for nested blocks/arrays.
    Json {
        /// `json_each` joins: `(source_expr, alias)`.
        each_joins: Vec<(String, String)>,
        /// Final expression, e.g. `json_extract(posts_content.data, '$.body')`.
        extract_expr: String,
        /// Leaf field type for operand coercion. `None` falls back to Text.
        field_type: Option<FieldType>,
        /// The list the leaf holds — a scalar has-many list or a has-many
        /// reference's id list — whose filter quantifies over its elements.
        list: Option<ListLeaf>,
    },
}

/// Result of walking a filter path through a row's JSON: the `json_each`
/// joins needed, the final extract expression, the leaf field type for
/// binding, and the list the leaf holds, if any.
pub(super) type JsonWalkResult = (
    Vec<(String, String)>,
    String,
    Option<FieldType>,
    Option<ListLeaf>,
);
