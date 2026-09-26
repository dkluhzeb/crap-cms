//! The stored form of one has-many value: what a covered leaf keeps, and the
//! change a stored value — a column's, or JSON holding lists — needs to reach
//! it.

use serde_json::{Map, Value, from_str};

use crate::{
    core::{FieldDefinition, VisitAction, walk_nested_mut},
    db::{
        DbValue,
        query::helpers::{ListPlace, stored_list},
    },
};

/// The leaves a pass covers: scalar has-many fields, and references wherever
/// they are stored in a row rather than a junction table — a has-many one as
/// its id list, a has-one one as its single id. (The pass only reaches a
/// reference inside a row: a document's own reference is carried between its
/// column and its junction by the cardinality pass.)
pub(super) fn is_list_leaf(field: &FieldDefinition) -> bool {
    field.is_has_many_scalar() || field.field_type.is_reference()
}

/// Whether `field` stores one reference, not a list.
fn is_single_reference(field: &FieldDefinition) -> bool {
    field.field_type.is_reference() && !field.is_has_many_reference()
}

/// The stored form of `value` for the covered leaf `field`: a has-one
/// reference's single id, else the list [`stored_list`] reads. `Err` holds the
/// values that don't fit.
fn stored_shape(
    field: &FieldDefinition,
    value: &Value,
    place: ListPlace,
) -> Result<Value, Vec<Value>> {
    if is_single_reference(field) {
        return stored_single(value);
    }

    stored_list(field, value, place)
}

/// A has-one reference's stored form: a list (or JSON-array text, as a row's
/// column holds it) of at most one id unwrapped — an empty one to NULL — and
/// anything else as it is. A list of several ids is refused.
fn stored_single(value: &Value) -> Result<Value, Vec<Value>> {
    match value {
        Value::Array(items) => match items.as_slice() {
            [] => Ok(Value::Null),
            [one] => Ok(one.clone()),
            _ => Err(items.clone()),
        },
        Value::String(text) if text.trim_start().starts_with('[') => {
            match from_str::<Value>(text) {
                Ok(list @ Value::Array(_)) => stored_single(&list),
                _ => Ok(value.clone()),
            }
        }
        other => Ok(other.clone()),
    }
}

/// Why `field`'s stored value was refused: the elements a list can't hold, or
/// the ids a has-one reference can't keep all of.
fn rejected_value(field: &FieldDefinition, elements: &[Value]) -> String {
    if !is_single_reference(field) {
        return rejected_elements(elements);
    }

    format!(
        "holds {} values, but the field is has-one and keeps one: {}",
        elements.len(),
        rejected_elements(elements)
    )
}

/// What a row's stored value needs.
pub(super) enum Change {
    /// It is in its list form already.
    Keep,
    /// Store this value instead.
    Store(DbValue),
    /// These values hold nothing of their field's type.
    Rejected(Vec<String>),
}

/// The change the stored value of a covered column at `place` needs.
pub(super) fn column_change(field: &FieldDefinition, stored: &DbValue, place: ListPlace) -> Change {
    let list = match stored_shape(field, &stored.to_json(), place) {
        Ok(list) => list,
        Err(elements) => return Change::Rejected(vec![rejected_value(field, &elements)]),
    };

    if list.is_null() {
        return Change::Store(DbValue::Null);
    }

    // A single reference is its id as text; a list, its JSON.
    let text = match list {
        Value::String(id) => id,
        list => list.to_string(),
    };

    if matches!(stored, DbValue::Text(raw) if *raw == text) {
        return Change::Keep;
    }

    Change::Store(DbValue::Text(text))
}

/// The change a JSON-stored value needs for the has-many lists it holds — the
/// value of the field `name` when given, otherwise an object of `fields`.
/// Text that isn't JSON of that shape holds no list to change.
pub(super) fn json_change(
    stored: &DbValue,
    fields: &[FieldDefinition],
    name: Option<&str>,
) -> Change {
    let DbValue::Text(raw) = stored else {
        return Change::Keep;
    };
    let Ok(value) = from_str::<Value>(raw) else {
        return Change::Keep;
    };

    let mut data = match (name, value) {
        (Some(name), value) => Map::from_iter([(name.to_string(), value)]),
        (None, Value::Object(map)) => map,
        (None, _) => return Change::Keep,
    };

    let mut rejected = Vec::new();
    let changed = normalize_nested(&mut data, fields, &mut rejected);

    if !rejected.is_empty() {
        return Change::Rejected(rejected);
    }

    if !changed {
        return Change::Keep;
    }

    let stored = match name {
        Some(name) => data.remove(name).unwrap_or(Value::Null),
        None => Value::Object(data),
    };

    Change::Store(DbValue::Text(stored.to_string()))
}

/// Bring every has-many list inside `data`, at any depth, to its list form.
/// Records each value that can't be as `field: elements`, and returns whether
/// anything changed.
fn normalize_nested(
    data: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    rejected: &mut Vec<String>,
) -> bool {
    let mut changed = false;

    walk_nested_mut(data, fields, &mut Vec::new(), &mut |field, level, _| {
        if !is_list_leaf(field) {
            return VisitAction::Keep;
        }

        let Some(value) = level.root_get(&field.name) else {
            return VisitAction::Keep;
        };

        match stored_shape(field, value, ListPlace::Row) {
            Ok(list) if list == *value => VisitAction::Keep,
            Ok(list) => {
                changed = true;
                VisitAction::Replace(list)
            }
            Err(elements) => {
                rejected.push(format!(
                    "{} {}",
                    field.name,
                    rejected_value(field, &elements)
                ));
                VisitAction::Keep
            }
        }
    });

    changed
}

/// The elements a list can't hold, as JSON.
fn rejected_elements(elements: &[Value]) -> String {
    let shown: Vec<String> = elements.iter().map(Value::to_string).collect();

    format!("holds {}", shown.join(", "))
}
