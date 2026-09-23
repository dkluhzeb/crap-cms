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
        /// When the join table has a `_locale` column and the query is
        /// scoped to a single locale, this holds the locale string to
        /// constrain the subquery with `_locale = ?`. `None` means no
        /// locale filtering (junction table has no `_locale` column, or
        /// `LocaleMode::All` is active).
        locale_constraint: Option<String>,
    },
}

/// How to access the filtered value within a subquery.
#[derive(Debug)]
pub(in crate::db::query::filter) enum SubqueryCondition {
    /// Direct column on an array join table (an array sub-field).
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
    /// `json_extract` on the `data` column, possibly with `json_each` joins
    /// for nested blocks/arrays.
    Json {
        /// `json_each` joins: `(source_expr, alias)`.
        each_joins: Vec<(String, String)>,
        /// Final expression, e.g. `json_extract(data, '$.body')`.
        extract_expr: String,
        /// Leaf field type for operand coercion. `None` falls back to Text.
        field_type: Option<FieldType>,
        /// The list the leaf holds — a scalar has-many list or a has-many
        /// reference's id list — whose filter quantifies over its elements.
        list: Option<ListLeaf>,
    },
}

/// Result of walking a block filter path: the `json_each` joins needed,
/// the final extract expression, the leaf field type for binding, and the
/// list the leaf holds, if any.
pub(super) type BlockWalkResult = (
    Vec<(String, String)>,
    String,
    Option<FieldType>,
    Option<ListLeaf>,
);
