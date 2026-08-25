//! Group field representation: convert between the **nested** canonical
//! in-memory form (`{ seo: { meta_title } }`) and the **flat** DB-column form
//! (`{ seo__meta_title }`).
//!
//! Nested is the single canonical shape everywhere above the `db/query` layer
//! (ingress, hooks, validation, field access). Flat `group__sub` is purely a
//! DB-column encoding, applied at the persistence edge: [`flatten_group_fields`]
//! is the write-side boundary (nested → columns), the mirror of the read-side
//! [`crate::db::query`] hydration (`reconstruct_group_fields`, columns →
//! nested). [`nest_group_fields`] normalizes ingress data (flat form inputs,
//! flattened Lua tables) to the canonical nested shape.
//!
//! Both are **idempotent**: `flatten(flat) == flat`, `nest(nested) == nested`,
//! and `flatten(nest(flat)) == flat` (round-trip). Only `Group` fields nest;
//! non-group object values (a `Json` field, an array/blocks value) and companion
//! columns (a `Date` field's `__tz` sibling) pass through untouched.

use serde_json::{Map, Value};

use crate::core::{DocumentFields, FieldDefinition, FieldType, find_field, prefixed_name};

/// Flatten nested `Group` objects into `group__sub` column keys. The inverse of
/// [`nest_group_fields`]; the write-side boundary used by the DB persist layer.
///
/// A key is flattened only when it names a `Group` field whose value is an
/// object; otherwise it passes through (already-flat data is unchanged, and
/// non-group object values — a top-level `Json` field — are preserved).
#[must_use]
pub(crate) fn flatten_group_fields(
    data: &DocumentFields,
    fields: &[FieldDefinition],
) -> DocumentFields {
    let mut map = DocumentFields::new();

    for (k, v) in data {
        if let Some(field) = find_field(k, fields)
            && field.field_type == FieldType::Group
            && let Some(obj) = v.as_object()
        {
            flatten_group_obj(k, obj, &field.fields, &mut map);

            continue;
        }

        map.insert(k.clone(), v.clone());
    }

    map
}

/// Flatten a group object into `prefix__key` pairs, recursing ONLY into nested
/// `Group` sub-fields. A non-group object value (a `Json` field, an array) is
/// kept whole — recursing would flatten it onto a non-existent
/// `prefix__sub__key` column and drop it on persist. A `Date` field's `__tz`
/// companion key has no field definition, so it passes through as a flat sibling
/// (`prefix__date__tz`), round-tripping with [`nest_group_fields`].
fn flatten_group_obj(
    prefix: &str,
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    map: &mut DocumentFields,
) {
    for (sub_key, sub_val) in obj {
        let flat_key = prefixed_name(prefix, sub_key);

        if let Some(f) = find_field(sub_key, fields)
            && f.field_type == FieldType::Group
            && let Value::Object(nested) = sub_val
        {
            flatten_group_obj(&flat_key, nested, &f.fields, map);
        } else {
            map.insert(flat_key, sub_val.clone());
        }
    }
}

/// Normalize a data map to the canonical nested shape: flat `group__sub` column
/// keys become nested `{ group: { sub } }` objects. The inverse of
/// [`flatten_group_fields`]; used to normalize ingress (admin form scalars,
/// flattened Lua tables). Already-nested data and non-group keys pass through,
/// so it is idempotent.
#[must_use]
pub(crate) fn nest_group_fields(
    data: &DocumentFields,
    fields: &[FieldDefinition],
) -> DocumentFields {
    let mut out = DocumentFields::new();

    for (k, v) in data {
        let (path, leaf) = group_prefix_path(k, fields);
        insert_at_path(&mut out, &path, &leaf, v.clone());
    }

    out
}

/// Split a flat key into the longest leading chain of `Group` segments (the
/// nesting path) plus the remaining key placed at that level. A segment is part
/// of the path only if it names a `Group` field *and* is not the final segment
/// (a group is always addressed through a sub-field). Non-group leading segments
/// (e.g. a `Date` field's `date__tz` companion, or a plain key) yield an empty
/// path and the whole key as the leaf, so they pass through unchanged.
fn group_prefix_path(key: &str, fields: &[FieldDefinition]) -> (Vec<String>, String) {
    let segs: Vec<&str> = key.split("__").collect();
    let mut path = Vec::new();
    let mut cur_fields = fields;
    let mut i = 0;

    while i + 1 < segs.len() {
        match find_field(segs[i], cur_fields) {
            Some(f) if f.field_type == FieldType::Group => {
                path.push(segs[i].to_string());
                cur_fields = &f.fields;
                i += 1;
            }
            _ => break,
        }
    }

    (path, segs[i..].join("__"))
}

/// Insert `value` at `leaf` within the nested object reached by following `path`
/// (creating intermediate objects). An empty path inserts at the top level.
fn insert_at_path(out: &mut DocumentFields, path: &[String], leaf: &str, value: Value) {
    let Some((first, rest)) = path.split_first() else {
        out.insert(leaf.to_string(), value);
        return;
    };

    let mut cur = out
        .entry(first.clone())
        .or_insert_with(|| Value::Object(Map::new()));

    for seg in rest {
        let Value::Object(obj) = cur else {
            return; // conflict with a non-object value — bail rather than clobber
        };
        cur = obj
            .entry(seg.clone())
            .or_insert_with(|| Value::Object(Map::new()));
    }

    if let Value::Object(obj) = cur {
        obj.insert(leaf.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn group(name: &str, sub: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Group)
            .fields(sub)
            .build()
    }

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn schema() -> Vec<FieldDefinition> {
        vec![
            text("title"),
            group(
                "seo",
                vec![text("meta_title"), group("social", vec![text("og_title")])],
            ),
        ]
    }

    fn fields_from(data: &[(&str, Value)]) -> DocumentFields {
        data.iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn flatten_nests_and_back_roundtrips() {
        let nested = fields_from(&[
            ("title", json!("Hi")),
            (
                "seo",
                json!({ "meta_title": "T", "social": { "og_title": "OG" } }),
            ),
        ]);

        let flat = flatten_group_fields(&nested, &schema());
        assert_eq!(flat.get("title"), Some(&json!("Hi")));
        assert_eq!(flat.get("seo__meta_title"), Some(&json!("T")));
        assert_eq!(flat.get("seo__social__og_title"), Some(&json!("OG")));
        assert!(!flat.contains_key("seo"));

        let renested = nest_group_fields(&flat, &schema());
        assert_eq!(renested.get("title"), Some(&json!("Hi")));
        assert_eq!(
            renested.get("seo"),
            Some(&json!({ "meta_title": "T", "social": { "og_title": "OG" } }))
        );
    }

    #[test]
    fn nest_from_flat_form_input() {
        let flat = fields_from(&[
            ("title", json!("Hi")),
            ("seo__meta_title", json!("T")),
            ("seo__social__og_title", json!("OG")),
        ]);

        let nested = nest_group_fields(&flat, &schema());
        assert_eq!(
            nested.get("seo"),
            Some(&json!({ "meta_title": "T", "social": { "og_title": "OG" } }))
        );
        assert_eq!(nested.get("title"), Some(&json!("Hi")));
    }

    #[test]
    fn both_are_idempotent() {
        let flat = fields_from(&[("seo__meta_title", json!("T"))]);
        let f = &schema();
        assert_eq!(
            flatten_group_fields(&flatten_group_fields(&flat, f), f),
            flatten_group_fields(&flat, f)
        );

        let nested = nest_group_fields(&flat, f);
        assert_eq!(nest_group_fields(&nested, f), nested);
    }

    #[test]
    fn date_tz_companion_passes_through_both_ways() {
        // A Date field with a `__tz` companion column: `published__tz` is NOT a
        // group path (`published` is a leaf), so it stays a flat sibling inside
        // its group level and round-trips.
        let f = vec![group(
            "meta",
            vec![FieldDefinition::builder("published", FieldType::Date).build()],
        )];

        // Nested form carries the tz as a flat sibling within the group object.
        let nested = fields_from(&[(
            "meta",
            json!({ "published": "2026-01-01", "published__tz": "+02:00" }),
        )]);
        let flat = flatten_group_fields(&nested, &f);
        assert_eq!(flat.get("meta__published"), Some(&json!("2026-01-01")));
        assert_eq!(flat.get("meta__published__tz"), Some(&json!("+02:00")));

        let renested = nest_group_fields(&flat, &f);
        assert_eq!(
            renested.get("meta"),
            Some(&json!({ "published": "2026-01-01", "published__tz": "+02:00" }))
        );
    }

    #[test]
    fn top_level_tz_companion_and_unknown_keys_pass_through() {
        let f = vec![FieldDefinition::builder("published", FieldType::Date).build()];
        let flat = fields_from(&[
            ("published", json!("2026-01-01")),
            ("published__tz", json!("+02:00")),
            ("unknown", json!(1)),
        ]);

        let nested = nest_group_fields(&flat, &f);
        assert_eq!(nested.get("published"), Some(&json!("2026-01-01")));
        assert_eq!(nested.get("published__tz"), Some(&json!("+02:00")));
        assert_eq!(nested.get("unknown"), Some(&json!(1)));
    }

    #[test]
    fn non_group_object_value_is_not_flattened() {
        // A Json field holding an object must stay whole, not be flattened.
        let f = vec![FieldDefinition::builder("payload", FieldType::Json).build()];
        let nested = fields_from(&[("payload", json!({ "a": { "b": 1 } }))]);

        let flat = flatten_group_fields(&nested, &f);
        assert_eq!(flat.get("payload"), Some(&json!({ "a": { "b": 1 } })));
        assert!(!flat.contains_key("payload__a"));
    }

    #[test]
    fn group_under_transparent_row_flattens() {
        let f = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![group("seo", vec![text("meta_title")])])
                .build(),
        ];
        let nested = fields_from(&[("seo", json!({ "meta_title": "T" }))]);

        let flat = flatten_group_fields(&nested, &f);
        assert_eq!(flat.get("seo__meta_title"), Some(&json!("T")));

        let renested = nest_group_fields(&flat, &f);
        assert_eq!(renested.get("seo"), Some(&json!({ "meta_title": "T" })));
    }
}
