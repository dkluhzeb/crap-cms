//! The shared edge helpers the query layer builds its SQL and its values from.
//!
//! - `limits` -- pagination, population-depth and optional-limit clamping.
//! - `date` -- date normalization to stored UTC form, timezone conversion and
//!   the current timestamp.
//! - `coerce` / `coerce_json` -- form-string and typed-JSON coercion to
//!   database values, with the NUL-byte guard.
//! - `has_many` -- scalar has-many list encoding (write) and parsing (read).
//! - `encode` -- the one column encoding and read decoding of a field's value,
//!   and the typed form a value takes inside a JSON-stored row.
//! - `document` -- rewriting a document's values (and its array rows) into the
//!   form their writes store.
//! - `naming` -- generated column and table names: locale-suffixed columns, the
//!   `_tz` / `_lang` companion columns, join / global / version tables.
//! - `sql` -- SQL text building: identifier quoting, `LIKE` escaping, `WHERE`
//!   assembly, the soft-delete predicate and placeholder lists.

mod coerce;
mod coerce_json;
mod date;
mod document;
mod encode;
mod has_many;
mod limits;
mod locale_clause;
mod naming;
mod sql;

pub use date::utc_to_local;
pub use limits::{apply_pagination_limits, clamp_depth, floor_optional_limit};

pub(crate) use coerce::{coerce_date_value, coerce_value, validate_no_null_byte};
pub(crate) use coerce_json::{
    coerce_date_value_json, coerce_json_value, validate_no_null_byte_json,
};
pub(crate) use date::{DayRange, normalize_date_value, normalize_date_with_timezone, utc_now};
pub(crate) use document::stored_document_values;
pub(in crate::db::query) use encode::companion_writes;
pub(crate) use encode::{
    column_value, companion_value, decode_row_value, decode_value, decodes, nested_value,
    row_column_value, stored_value,
};
pub(crate) use has_many::{
    ListPlace, coerce_has_many_scalar, is_polymorphic, number_element, parse_has_many_scalar,
    reference_list, stored_list,
};
pub(in crate::db::query) use locale_clause::outside_locales_clause;
pub(crate) use naming::{
    column_belongs_to, global_table, join_table, lang_column, locale_column, tz_column,
    versions_table,
};
pub(crate) use sql::{
    SOFT_DELETE_ACTIVE, append_soft_delete_filter, append_sql_condition, like_escape,
    placeholder_list, qualified_ident, quote_ident,
};

// The field-tree walkers `prefixed_name` and `walk_leaf_fields` now live in
// `core::walk` (the single home for every field-tree traversal). Re-exported
// here so the many `query::helpers::{prefixed_name, walk_leaf_fields}` call
// sites keep their import path.
pub(crate) use crate::core::walk::{prefixed_name, walk_leaf_fields};
