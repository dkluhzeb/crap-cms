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
    /// A value inside a row's JSON — a block row's `data`, an array row's
    /// group / nested array / nested blocks column — read one way per block
    /// type where block types define the path differently (see
    /// [`JsonLeaf`]). A row matches when one of the readings holds.
    Json(Vec<JsonLeaf>),
}

/// One reading of a filter path inside a row's JSON: the steps from the
/// join-table row down to the value, the final extract expression, the leaf
/// type for binding, and the list the leaf holds, if any.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::db::query::filter) struct JsonLeaf {
    /// `json_each` expansions and block-type conditions, in path order.
    pub(in crate::db::query::filter) steps: Vec<JsonStep>,
    /// Final expression, e.g. `json_extract(posts_content.data, '$.body')`.
    pub(in crate::db::query::filter) extract_expr: String,
    /// Leaf field type for operand coercion. `None` falls back to Text.
    pub(in crate::db::query::filter) field_type: Option<FieldType>,
    /// The list the leaf holds — a scalar has-many list or a has-many
    /// reference's id list — whose filter quantifies over its elements.
    pub(in crate::db::query::filter) list: Option<ListLeaf>,
}

impl JsonLeaf {
    /// The `json_each` expansions, as `(source_expr, alias)`.
    pub(in crate::db::query::filter) fn each_joins(&self) -> Vec<(&str, &str)> {
        self.steps
            .iter()
            .filter_map(|step| match step {
                JsonStep::Each { source, alias } => Some((source.as_str(), alias.as_str())),
                JsonStep::BlockType { .. } | JsonStep::OtherBlockType { .. } => None,
            })
            .collect()
    }

    /// Whether this is the reading of rows whose block type declares no field
    /// of the filtered name — the value is absent there.
    pub(in crate::db::query::filter) fn reads_absent(&self) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step, JsonStep::OtherBlockType { .. }))
    }

    /// Whether the reading holds in rows of every block type.
    pub(in crate::db::query::filter) fn is_unconditional(&self) -> bool {
        self.steps
            .iter()
            .all(|step| matches!(step, JsonStep::Each { .. }))
    }
}

/// One step of a row path on the way to its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::db::query::filter) enum JsonStep {
    /// Expand the rows of the JSON array `source` as `alias`.
    Each { source: String, alias: String },
    /// Only a block row whose type — read by `expr` — is `block_type`.
    BlockType { expr: String, block_type: String },
    /// Only a block row whose type — read by `expr` — is none of `declared`
    /// (or missing).
    OtherBlockType { expr: String, declared: Vec<String> },
}
