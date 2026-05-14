//! Resolved filter shape returned to the WHERE-clause builder.

use crate::core::FieldType;

/// A filter resolved to its SQL representation.
#[derive(Debug)]
pub(in crate::db::query::filter) enum ResolvedFilter {
    /// Direct column on parent table (existing behavior).
    ///
    /// `field_type` is the leaf field's type, used to cast filter operand
    /// values when binding. `None` when the type cannot be determined —
    /// binding falls back to `DbValue::Text`.
    Column {
        col: String,
        field_type: Option<FieldType>,
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
    /// Direct column on join table (array sub-fields, has-many `related_id`).
    ///
    /// `field_type` drives operand casting; `None` means fall back to Text.
    Column {
        col: String,
        field_type: Option<FieldType>,
    },
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
    },
}

/// Result of walking a block filter path: the `json_each` joins needed,
/// the final extract expression, and the leaf field type for binding.
pub(super) type BlockWalkResult = (Vec<(String, String)>, String, Option<FieldType>);
