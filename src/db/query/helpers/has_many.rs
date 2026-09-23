//! Has-many list encoding (write) and parsing (read): scalar has-many lists,
//! and the id lists of has-many references stored inside rows.
//!
//! Every write stores a list as a JSON array. Text that isn't one reads by
//! where it is stored ([`ListPlace`]): in a document's own column it is one
//! value — a single value kept from before its field held a list — and inside
//! an array or blocks row it is a comma-separated list, the form earlier admin
//! forms stored a row's list in.

use serde_json::{Value, from_str};

use crate::{
    core::{FieldDefinition, FieldType, RelationshipConfig, parse_number, reference_items},
    db::{DbValue, query::poly_ref, types::real_to_json_number},
};

/// Where a stored has-many list lives, which decides how text that isn't a JSON
/// array reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListPlace {
    /// A document's own column — top-level, per locale, or a group's prefixed
    /// column. Every release stored a list there as a JSON array, so other text
    /// is one value: `"Hello, world"` is `["Hello, world"]`.
    Column,
    /// Inside an array or blocks row — an array table's column or a row's JSON.
    /// Earlier admin forms stored a list there as comma-separated text, so other
    /// text reads as comma-separated values: `"a, b"` is `["a","b"]`.
    Row,
}

/// Canonicalize a **scalar has-many** field's value into the JSON-array TEXT
/// stored in its column (see [`FieldDefinition::is_has_many_scalar`]).
///
/// Accepts the two shapes that reach the write edge — a typed `Value::Array`
/// (gRPC / Lua / MCP) and a JSON-array *string* (admin form, pre-normalized by
/// `transform_select_has_many`) — and maps every element to the field's own type
/// so the stored element types are identical regardless of ingress surface:
/// `Number` → JSON number (whole values as integers, non-numeric elements
/// dropped), every other scalar (`Text` / `Select` / `Radio`) → string. A
/// single number or boolean — one an import carries, say — is a one-element
/// list, an object no list at all; other text reads by `place`. A null value
/// stores SQL `NULL`; any present value stores a JSON array (`[]` when empty).
/// The read path reverses this via [`parse_has_many_scalar`].
///
/// [`FieldDefinition::is_has_many_scalar`]: crate::core::FieldDefinition::is_has_many_scalar
pub(crate) fn coerce_has_many_scalar(
    field_type: &FieldType,
    val: &Value,
    place: ListPlace,
) -> DbValue {
    if val.is_null() {
        return DbValue::Null;
    }

    DbValue::Text(canonical_list(field_type, &list_elements(val, place)).to_string())
}

/// The elements a has-many value holds before they take the field's type: a
/// list's own, those of text spelling a list (read by `place`), or the one a
/// single number or boolean is. An object is no value of a scalar list and
/// holds none.
fn list_elements(val: &Value, place: ListPlace) -> Vec<Value> {
    match val {
        Value::Null | Value::Object(_) => Vec::new(),
        Value::Array(items) => items.clone(),
        Value::String(text) => text_list_elements(text, place),
        single => vec![single.clone()],
    }
}

/// The list a stored has-many value holds once stored the way a write stores
/// it — the reading the schema sync applies to a value stored before its field
/// held this list: a field switched to `has_many` over its single values, or a
/// list whose element type changed.
///
/// A list, or text spelling one — a JSON array, or inside a row comma-separated
/// values: the reading [`coerce_has_many_scalar`] gives a write at `place` —
/// keeps its elements; any other single value, other text in a column
/// included, becomes a one-element list. Null and blank text hold no list and
/// stay `Null`.
///
/// # Errors
///
/// Returns the elements a write would drop for holding no value of the field's
/// type — text in a number list — since the value can't become a list without
/// losing them; an object is handed back whole.
pub(crate) fn stored_has_many_list(
    field_type: &FieldType,
    value: &Value,
    place: ListPlace,
) -> Result<Value, Vec<Value>> {
    if value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty()) {
        return Ok(Value::Null);
    }

    if value.is_object() {
        return Err(vec![value.clone()]);
    }

    let elements = list_elements(value, place);

    let rejected: Vec<Value> = elements
        .iter()
        .filter(|el| !el.is_null() && canonical_has_many_element(field_type, el).is_none())
        .cloned()
        .collect();

    if !rejected.is_empty() {
        return Err(rejected);
    }

    Ok(canonical_list(field_type, &elements))
}

/// The id list a has-many reference stored inside a row holds: the ids its
/// value carries — a list, a JSON array spelled as text, or the admin form's
/// comma list — as strings, the reading the junction writer gives a top-level
/// reference. An entry that isn't an id (a polymorphic one not spelling
/// `collection/id`) is dropped. A null value stays `Null`.
pub(crate) fn reference_list(value: &Value, polymorphic: bool) -> Value {
    if value.is_null() {
        return Value::Null;
    }

    Value::Array(
        reference_items(value)
            .into_iter()
            .filter(|item| is_reference(item, polymorphic))
            .collect(),
    )
}

/// [`reference_list`] for a value stored before its field held this list — a
/// reference switched to `has_many` over its single id, or a list the admin
/// form stored as comma-separated ids. Null and blank text stay `Null`, a
/// single id becomes a one-element list.
///
/// # Errors
///
/// Returns the entries that aren't ids, which [`reference_list`] would drop.
pub(crate) fn stored_reference_list(value: &Value, polymorphic: bool) -> Result<Value, Vec<Value>> {
    if value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty()) {
        return Ok(Value::Null);
    }

    let items = match value {
        Value::Array(_) | Value::String(_) => reference_items(value),
        single => vec![single.clone()],
    };

    let rejected: Vec<Value> = items
        .into_iter()
        .filter(|item| !is_reference(item, polymorphic))
        .collect();

    if !rejected.is_empty() {
        return Err(rejected);
    }

    Ok(reference_list(value, polymorphic))
}

/// The list a stored value of the list field `field` — a scalar has-many field
/// or a has-many reference — holds once stored at `place` as a write stores it.
/// A reference list is only ever stored inside a row, so it always reads a
/// comma-separated text as its ids.
///
/// # Errors
///
/// Returns the elements the list can't hold (see [`stored_has_many_list`] and
/// [`stored_reference_list`]).
pub(crate) fn stored_list(
    field: &FieldDefinition,
    value: &Value,
    place: ListPlace,
) -> Result<Value, Vec<Value>> {
    if field.is_has_many_reference() {
        return stored_reference_list(value, is_polymorphic(field));
    }

    stored_has_many_list(&field.field_type, value, place)
}

/// Whether `field` references documents of more than one collection, its ids
/// spelled `collection/id`.
pub(crate) fn is_polymorphic(field: &FieldDefinition) -> bool {
    field
        .relationship
        .as_ref()
        .is_some_and(RelationshipConfig::is_polymorphic)
}

/// Whether `item` is an entry of a reference list: an id, spelled
/// `collection/id` when the reference is polymorphic.
fn is_reference(item: &Value, polymorphic: bool) -> bool {
    let Some(text) = item.as_str() else {
        return false;
    };

    !polymorphic || poly_ref::parse(text).is_some()
}

/// `elements` mapped to the field's canonical element type, the ones holding
/// no value of it dropped.
fn canonical_list(field_type: &FieldType, elements: &[Value]) -> Value {
    Value::Array(
        elements
            .iter()
            .filter_map(|el| canonical_has_many_element(field_type, el))
            .collect(),
    )
}

/// The elements a has-many list spelled as text holds: none for blank text, a
/// JSON array's elements (commas inside an element kept). Other text is, in a
/// column, the one value it spells; inside a row — where earlier admin forms
/// stored a list as comma-separated values — its comma-separated values,
/// trimmed, empties dropped.
fn text_list_elements(text: &str, place: ListPlace) -> Vec<Value> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    if let Ok(elements) = from_str::<Vec<Value>>(text) {
        return elements;
    }

    if place == ListPlace::Column {
        return vec![Value::String(text.to_string())];
    }

    text.split(',')
        .map(str::trim)
        .filter(|el| !el.is_empty())
        .map(|el| Value::String(el.to_string()))
        .collect()
}

/// The number a Number value holds — a single value or a has-many element: a
/// JSON number, or a number string read by [`parse_number`] (surrounding
/// whitespace ignored). The one reading validation and the write share.
pub(crate) fn number_element(el: &Value) -> Option<f64> {
    match el {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_number(s),
        _ => None,
    }
}

/// Map one has-many element to the field's canonical JSON type, or `None` to
/// drop it (a non-numeric element in a `Number` list, or a null), mirroring the
/// single-value coercion's "invalid ⇒ dropped" rule.
fn canonical_has_many_element(field_type: &FieldType, el: &Value) -> Option<Value> {
    if el.is_null() {
        return None;
    }

    if *field_type == FieldType::Number {
        let n = number_element(el)?;

        return n.is_finite().then(|| real_to_json_number(n));
    }

    // Text / Select / Radio → string form.
    let s = match el {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };

    Some(Value::String(s))
}

/// Parse a **scalar has-many** column's stored TEXT back into a typed JSON array
/// on read — the inverse of [`coerce_has_many_scalar`]. `row_to_document` is
/// type-blind and yields the raw string, so every read surface would otherwise
/// see `"[1,2]"` instead of `[1, 2]`. A SQL `NULL` (absent value) stays `Null`,
/// an object holds no elements; any other value reads as the list a write of it
/// at `place` stores — so a value kept from before its field held a list (in a
/// version snapshot, say) reads as the list the schema sync stores for it,
/// never as the raw string.
#[must_use]
pub(crate) fn parse_has_many_scalar(
    field_type: &FieldType,
    val: &Value,
    place: ListPlace,
) -> Value {
    if val.is_null() {
        return Value::Null;
    }

    canonical_list(field_type, &list_elements(val, place))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── scalar has-many coercion (write) + parse (read) ─────────────────

    /// Regression: an earlier admin form stored a has-many list inside an
    /// array or blocks row as comma-separated text (`"a,b"`); the JSON-only
    /// reading turned it into an empty list, so the one-time conversion of
    /// nested values erased every such selection. Both forms read the same.
    #[test]
    fn a_comma_separated_list_in_a_row_reads_as_its_elements() {
        let in_row = |ft: &FieldType, v: Value| stored_at(ft, &v, ListPlace::Row);

        assert_eq!(in_row(&FieldType::Text, json!("a,b")), r#"["a","b"]"#);
        assert_eq!(in_row(&FieldType::Text, json!(" a , b ")), r#"["a","b"]"#);
        assert_eq!(in_row(&FieldType::Number, json!("1,2.5")), "[1,2.5]");
        assert_eq!(in_row(&FieldType::Text, json!("single")), r#"["single"]"#);
        assert_eq!(in_row(&FieldType::Text, json!("")), "[]");
        // A JSON array keeps its exact elements, commas included.
        assert_eq!(in_row(&FieldType::Text, json!(r#"["a,b"]"#)), r#"["a,b"]"#);
    }

    /// Regression: a column's single text value was split at its commas when
    /// its field switched to `has_many` (`"Hello, world"` became `["Hello",
    /// "world"]`), though no release stored a column's list as comma text. In
    /// a column, text that isn't a JSON array is one value.
    #[test]
    fn text_in_a_column_is_one_value() {
        let in_column = |ft: &FieldType, v: Value| stored(ft, &v);

        assert_eq!(
            in_column(&FieldType::Text, json!("Hello, world")),
            r#"["Hello, world"]"#
        );
        assert_eq!(in_column(&FieldType::Text, json!("")), "[]");
        assert_eq!(in_column(&FieldType::Number, json!("5")), "[5]");
        assert_eq!(
            in_column(&FieldType::Text, json!(r#"["a","b"]"#)),
            r#"["a","b"]"#
        );
        assert_eq!(
            stored_has_many_list(&FieldType::Text, &json!("Hello, world"), ListPlace::Column),
            Ok(json!(["Hello, world"]))
        );
        assert_eq!(
            parse_has_many_scalar(&FieldType::Text, &json!("Hello, world"), ListPlace::Column),
            json!(["Hello, world"])
        );
        assert_eq!(
            parse_has_many_scalar(&FieldType::Text, &json!("Hello, world"), ListPlace::Row),
            json!(["Hello", "world"])
        );
    }

    /// A number list in a column holding one text that isn't a number can't
    /// become a list without losing it.
    #[test]
    fn comma_text_in_a_number_column_is_rejected() {
        assert_eq!(
            stored_has_many_list(&FieldType::Number, &json!("1,2"), ListPlace::Column),
            Err(vec![json!("1,2")])
        );
    }

    fn stored_at(field_type: &FieldType, v: &Value, place: ListPlace) -> String {
        match coerce_has_many_scalar(field_type, v, place) {
            DbValue::Text(s) => s,
            other => panic!("expected Text, got {other:?}"),
        }
    }

    fn stored(field_type: &FieldType, v: &Value) -> String {
        stored_at(field_type, v, ListPlace::Column)
    }

    // ── reference lists stored inside rows ───────────────────────────────

    /// A reference list is stored as its ids, whatever spelling the write
    /// carried: a list, a JSON array as text, or the admin form's comma list.
    #[test]
    fn a_reference_list_stores_its_ids() {
        assert_eq!(reference_list(&json!(["a", "b"]), false), json!(["a", "b"]));
        assert_eq!(reference_list(&json!(r#"["a"]"#), false), json!(["a"]));
        assert_eq!(reference_list(&json!("a, b"), false), json!(["a", "b"]));
        assert_eq!(reference_list(&json!(""), false), json!([]));
        assert_eq!(reference_list(&Value::Null, false), Value::Null);
        assert_eq!(
            reference_list(&json!(["posts/a", "bad"]), true),
            json!(["posts/a"])
        );
    }

    /// A stored single id becomes a one-element list; an entry that isn't an
    /// id is handed back instead of dropped.
    #[test]
    fn a_stored_reference_becomes_a_list_or_is_rejected() {
        assert_eq!(stored_reference_list(&json!("a"), false), Ok(json!(["a"])));
        assert_eq!(stored_reference_list(&json!(" "), false), Ok(Value::Null));
        assert_eq!(
            stored_reference_list(&json!("posts/a"), true),
            Ok(json!(["posts/a"]))
        );
        assert_eq!(
            stored_reference_list(&json!("a"), true),
            Err(vec![json!("a")])
        );
        assert_eq!(stored_reference_list(&json!(5), false), Err(vec![json!(5)]));
    }

    // ── stored values brought to the list form ───────────────────────────

    /// A single value becomes a one-element list of the field's type; text
    /// spelling a list keeps its elements; null and blank text hold none.
    #[test]
    fn a_single_stored_value_becomes_a_one_element_list() {
        let list =
            |ft: &FieldType, v: Value| stored_has_many_list(ft, &v, ListPlace::Column).unwrap();

        assert_eq!(list(&FieldType::Text, json!("foo")), json!(["foo"]));
        assert_eq!(list(&FieldType::Number, json!("5")), json!([5]));
        assert_eq!(list(&FieldType::Number, json!(5.0)), json!([5]));
        assert_eq!(list(&FieldType::Text, json!(7)), json!(["7"]));
        assert_eq!(
            list(&FieldType::Select, json!(r#"["a","b"]"#)),
            json!(["a", "b"])
        );
        assert_eq!(list(&FieldType::Text, json!("a, b")), json!(["a, b"]));
        assert_eq!(
            stored_has_many_list(&FieldType::Text, &json!("a, b"), ListPlace::Row),
            Ok(json!(["a", "b"]))
        );
        assert_eq!(list(&FieldType::Text, json!([1, "x"])), json!(["1", "x"]));
        assert_eq!(list(&FieldType::Text, json!("  ")), Value::Null);
        assert_eq!(list(&FieldType::Number, Value::Null), Value::Null);
    }

    /// A value holding no value of a number list's type can't become a list
    /// without losing it, so its elements are handed back instead.
    #[test]
    fn text_in_a_number_list_is_rejected_not_dropped() {
        assert_eq!(
            stored_has_many_list(&FieldType::Number, &json!("abc"), ListPlace::Column),
            Err(vec![json!("abc")])
        );
        assert_eq!(
            stored_has_many_list(&FieldType::Number, &json!(["1", "x", null]), ListPlace::Row),
            Err(vec![json!("x")])
        );
        assert_eq!(
            stored_has_many_list(&FieldType::Number, &json!([1, null]), ListPlace::Row),
            Ok(json!([1]))
        );
    }

    /// Regression: a single value that is neither a list nor text — a number
    /// an import carries — stored an empty list, dropping it.
    #[test]
    fn a_single_non_text_value_is_stored_as_a_one_element_list() {
        assert_eq!(stored(&FieldType::Number, &json!(5)), "[5]");
        assert_eq!(stored(&FieldType::Text, &json!(5)), r#"["5"]"#);
    }

    #[test]
    fn has_many_number_typed_array_stays_numeric() {
        assert_eq!(stored(&FieldType::Number, &json!([1, 2, 3])), "[1,2,3]");
    }

    /// The admin form pre-normalizes to a JSON array of *strings*; a Number list
    /// must be re-typed to numbers so it matches the API-written shape.
    #[test]
    fn has_many_number_stringified_elements_become_numbers() {
        assert_eq!(stored(&FieldType::Number, &json!(["1", "2"])), "[1,2]");
    }

    /// Admin sends the whole value as a JSON-array *string*.
    #[test]
    fn has_many_number_json_string_input_is_parsed() {
        assert_eq!(stored(&FieldType::Number, &json!("[1, 2]")), "[1,2]");
    }

    #[test]
    fn has_many_number_whole_floats_serialize_as_integers() {
        assert_eq!(stored(&FieldType::Number, &json!([1.0, 2.5])), "[1,2.5]");
    }

    #[test]
    fn has_many_number_drops_non_numeric_elements() {
        assert_eq!(
            stored(&FieldType::Number, &json!([1, "x", null, 3])),
            "[1,3]"
        );
    }

    #[test]
    fn has_many_text_numbers_become_strings() {
        assert_eq!(stored(&FieldType::Text, &json!([1, 2])), r#"["1","2"]"#);
    }

    #[test]
    fn has_many_text_strings_stay_strings() {
        assert_eq!(
            stored(&FieldType::Select, &json!(["a", "b"])),
            r#"["a","b"]"#
        );
    }

    #[test]
    fn has_many_null_stores_sql_null() {
        assert_eq!(
            coerce_has_many_scalar(&FieldType::Number, &Value::Null, ListPlace::Column),
            DbValue::Null
        );
    }

    #[test]
    fn has_many_empty_array_stores_empty_json() {
        assert_eq!(stored(&FieldType::Number, &json!([])), "[]");
    }

    #[test]
    fn parse_has_many_number_string_to_typed_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!("[1,2,3]"), ListPlace::Column),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn parse_has_many_text_string_to_typed_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Text, &json!(r#"["a","b"]"#), ListPlace::Column),
            json!(["a", "b"])
        );
    }

    #[test]
    fn parse_has_many_null_stays_null() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &Value::Null, ListPlace::Column),
            Value::Null
        );
    }

    #[test]
    fn parse_has_many_array_passes_through() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!([1, 2]), ListPlace::Column),
            json!([1, 2])
        );
    }

    #[test]
    fn parse_has_many_text_that_holds_no_number_reads_as_an_empty_number_list() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!("not json"), ListPlace::Column),
            json!([])
        );
    }

    /// Regression: a single value kept from before its field held a list — a
    /// draft snapshot's `"news"` — read as an empty list, so the edit form
    /// showed no value and saving it dropped the value. It reads as the list a
    /// write of it stores.
    #[test]
    fn parse_has_many_single_value_reads_as_a_one_element_list() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Text, &json!("news"), ListPlace::Column),
            json!(["news"])
        );
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!(5), ListPlace::Column),
            json!([5])
        );
    }

    /// Write then read round-trips to a typed array, agreeing regardless of the
    /// input shape (typed vs admin-stringified).
    #[test]
    fn has_many_write_read_round_trip_agrees_across_shapes() {
        for input in [json!([1, 2]), json!(["1", "2"]), json!("[1,2]")] {
            let DbValue::Text(stored) =
                coerce_has_many_scalar(&FieldType::Number, &input, ListPlace::Column)
            else {
                panic!("expected Text");
            };
            let read = parse_has_many_scalar(
                &FieldType::Number,
                &Value::String(stored),
                ListPlace::Column,
            );
            assert_eq!(read, json!([1, 2]), "input {input:?}");
        }
    }
}
