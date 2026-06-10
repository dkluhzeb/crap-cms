//! Field-access denials — which fields to strip from read output / write input,
//! including fields nested inside array/blocks rows at any depth.
//!
//! Field-level access control evaluates each field's `access` function once and,
//! when denied, records a [`FieldDenial`]. The denial is then applied to the
//! document data, stripping the field. Top-level and group fields are flat
//! columns ([`FieldDenial::Flat`]); fields inside array/blocks rows live in
//! nested JSON and are stripped from every row ([`FieldDenial::Nested`]).

use std::collections::HashMap;

use serde_json::{Map, Value};

/// A JSON object that a denial can be applied to at the root level. Implemented
/// for both `HashMap<String, Value>` (a [`crate::core::DocumentFields`] map) and
/// `serde_json::Map<String, Value>` (a live-event payload), so the same strip
/// logic serves write input, read output, and broadcast events.
pub trait JsonRoot {
    fn root_get(&self, key: &str) -> Option<&Value>;
    fn root_get_mut(&mut self, key: &str) -> Option<&mut Value>;
    fn root_remove(&mut self, key: &str);
    fn root_insert(&mut self, key: String, value: Value);
}

impl JsonRoot for HashMap<String, Value> {
    fn root_get(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
    fn root_get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.get_mut(key)
    }
    fn root_remove(&mut self, key: &str) {
        self.remove(key);
    }
    fn root_insert(&mut self, key: String, value: Value) {
        self.insert(key, value);
    }
}

impl JsonRoot for Map<String, Value> {
    fn root_get(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
    fn root_get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.get_mut(key)
    }
    fn root_remove(&mut self, key: &str) {
        self.remove(key);
    }
    fn root_insert(&mut self, key: String, value: Value) {
        self.insert(key, value);
    }
}

/// A path segment when descending into a document's data to reach a denied
/// sub-field nested inside array/blocks rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenialSeg {
    /// Descend into the nested object stored under this key — a group inside a row.
    Group(String),
    /// Descend into the array stored under this key. `block_type` is `Some` for a
    /// Blocks field, restricting the strip to rows whose `_block_type` matches —
    /// so a denial on one block type does not strip a same-named field from
    /// sibling block types. `None` for a plain Array (applies to every row).
    Rows {
        key: String,
        block_type: Option<String>,
    },
}

/// A field denied by access control, to be stripped from a document's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldDenial {
    /// A document-level field: a flat column name, possibly a `__`-joined group
    /// subfield (e.g. `seo__title`). Stripped from both the flat key and, after
    /// hydration, the nested group object.
    Flat(String),
    /// A field nested inside array/blocks rows. `array_key` is the top-level
    /// array/blocks data key (flat, may be `__`-group-prefixed);
    /// `array_block_type` (Some for a top-level Blocks field) restricts to rows
    /// of that block type; `row_path` descends within each row (groups / nested
    /// arrays / blocks); `leaf` is removed from every reached object.
    Nested {
        array_key: String,
        array_block_type: Option<String>,
        row_path: Vec<DenialSeg>,
        leaf: String,
    },
}

impl From<String> for FieldDenial {
    fn from(name: String) -> Self {
        Self::Flat(name)
    }
}

impl From<&str> for FieldDenial {
    fn from(name: &str) -> Self {
        Self::Flat(name.to_string())
    }
}

impl FieldDenial {
    /// Render as a dotted field path — for the Lua `field_*_denied` API and
    /// logs. `Flat` returns the column name (e.g. `seo__title`); `Nested`
    /// joins the array key, within-row path, and leaf with `.`
    /// (e.g. `items.meta.secret`, `outer.inner.locked`).
    #[must_use]
    pub fn display_path(&self) -> String {
        match self {
            Self::Flat(name) => name.clone(),
            Self::Nested {
                array_key,
                row_path,
                leaf,
                ..
            } => {
                let mut parts = vec![array_key.clone()];
                for seg in row_path {
                    let (DenialSeg::Group(n) | DenialSeg::Rows { key: n, .. }) = seg;
                    parts.push(n.clone());
                }
                parts.push(leaf.clone());

                parts.join(".")
            }
        }
    }

    /// Strip this denied field from a root data map (a document's fields, write
    /// input, or live-event payload). Safe against both pre-hydration data
    /// (flat group columns) and post-hydration data (nested group objects).
    pub fn strip_from<M: JsonRoot>(&self, root: &mut M) {
        match self {
            Self::Flat(name) => strip_flat(root, name),
            Self::Nested {
                array_key,
                array_block_type,
                row_path,
                leaf,
            } => {
                let Some(array) = resolve_array_mut(root, array_key) else {
                    return;
                };

                for row in array.iter_mut() {
                    if let Value::Object(obj) = row
                        && block_matches(obj, array_block_type.as_deref())
                    {
                        strip_in_object(obj, row_path, leaf);
                    }
                }
            }
        }
    }
}

/// Whether a block row matches the optional `_block_type` filter (always true
/// for `None`, i.e. plain arrays).
fn block_matches(obj: &Map<String, Value>, block_type: Option<&str>) -> bool {
    match block_type {
        None => true,
        Some(bt) => obj.get("_block_type").and_then(Value::as_str) == Some(bt),
    }
}

/// Remove a flat field, handling both the flat key and the `__`-separated
/// nested group form (post-hydration data).
fn strip_flat<M: JsonRoot>(root: &mut M, name: &str) {
    root.root_remove(name);

    let segments: Vec<&str> = name.split("__").collect();
    if segments.len() < 2 {
        return;
    }

    let Some((&first, rest)) = segments.split_first() else {
        return;
    };
    if let Some(Value::Object(map)) = root.root_get_mut(first) {
        remove_nested(map, rest);
    }
}

/// Remove a `__`-path leaf from nested group objects.
fn remove_nested(map: &mut Map<String, Value>, segments: &[&str]) {
    let Some((&first, rest)) = segments.split_first() else {
        return;
    };

    if rest.is_empty() {
        map.remove(first);
        return;
    }

    if let Some(Value::Object(inner)) = map.get_mut(first) {
        remove_nested(inner, rest);
    }
}

/// Resolve the array at `array_key`: the flat key first (top-level array, or a
/// pre-hydration group-prefixed column), then a `__`-separated group-nested
/// path (post-hydration).
fn resolve_array_mut<'a, M: JsonRoot>(
    root: &'a mut M,
    array_key: &str,
) -> Option<&'a mut Vec<Value>> {
    if matches!(root.root_get(array_key), Some(Value::Array(_))) {
        return match root.root_get_mut(array_key) {
            Some(Value::Array(a)) => Some(a),
            _ => None,
        };
    }

    let segments: Vec<&str> = array_key.split("__").collect();
    let (&first, rest) = segments.split_first()?;
    if rest.is_empty() {
        return None;
    }

    match root.root_get_mut(first) {
        Some(Value::Object(o)) => resolve_array_in_map(o, rest),
        _ => None,
    }
}

/// Recurse a nested group object chain to the array at the final segment.
fn resolve_array_in_map<'a>(
    map: &'a mut Map<String, Value>,
    segments: &[&str],
) -> Option<&'a mut Vec<Value>> {
    let (&first, rest) = segments.split_first()?;

    if rest.is_empty() {
        return match map.get_mut(first) {
            Some(Value::Array(a)) => Some(a),
            _ => None,
        };
    }

    match map.get_mut(first) {
        Some(Value::Object(o)) => resolve_array_in_map(o, rest),
        _ => None,
    }
}

/// Within a row object, follow `row_path` (group objects / nested array rows)
/// and remove `leaf` from every reached object.
fn strip_in_object(obj: &mut Map<String, Value>, row_path: &[DenialSeg], leaf: &str) {
    let Some((seg, rest)) = row_path.split_first() else {
        obj.remove(leaf);
        return;
    };

    match seg {
        DenialSeg::Group(name) => {
            if let Some(Value::Object(inner)) = obj.get_mut(name) {
                strip_in_object(inner, rest, leaf);
            }
        }
        DenialSeg::Rows { key, block_type } => {
            if let Some(Value::Array(rows)) = obj.get_mut(key) {
                for row in rows.iter_mut() {
                    if let Value::Object(o) = row
                        && block_matches(o, block_type.as_deref())
                    {
                        strip_in_object(o, rest, leaf);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn root(v: Value) -> HashMap<String, Value> {
        serde_json::from_value(v).unwrap()
    }

    fn strip(root: &mut HashMap<String, Value>, d: &FieldDenial) {
        d.strip_from(root);
    }

    #[test]
    fn flat_removes_top_level_key() {
        let mut r = root(json!({ "secret": "x", "keep": "y" }));
        strip(&mut r, &"secret".into());
        assert!(!r.contains_key("secret"));
        assert!(r.contains_key("keep"));
    }

    #[test]
    fn flat_removes_pre_hydration_group_column() {
        // Write input: group fields are flat `group__child` columns.
        let mut r = root(json!({ "seo__title": "x", "seo__desc": "y" }));
        strip(&mut r, &"seo__title".into());
        assert!(!r.contains_key("seo__title"));
        assert!(r.contains_key("seo__desc"));
    }

    #[test]
    fn flat_removes_post_hydration_nested_group() {
        // Read output: groups are nested objects.
        let mut r = root(json!({ "seo": { "title": "x", "desc": "y" } }));
        strip(&mut r, &"seo__title".into());
        assert_eq!(r["seo"], json!({ "desc": "y" }));
    }

    #[test]
    fn flat_removes_deeply_nested_group() {
        let mut r = root(json!({ "a": { "b": { "c": "x", "d": "y" } } }));
        strip(&mut r, &"a__b__c".into());
        assert_eq!(r["a"], json!({ "b": { "d": "y" } }));
    }

    #[test]
    fn nested_strips_array_sub_field_from_every_row() {
        let mut r = root(json!({
            "items": [
                { "name": "a", "secret": "1" },
                { "name": "b", "secret": "2" }
            ]
        }));
        strip(
            &mut r,
            &FieldDenial::Nested {
                array_key: "items".into(),
                array_block_type: None,
                row_path: vec![],
                leaf: "secret".into(),
            },
        );
        assert_eq!(
            r["items"],
            json!([{ "name": "a" }, { "name": "b" }]),
            "secret must be stripped from every row"
        );
    }

    #[test]
    fn nested_strips_group_inside_array_row() {
        let mut r = root(json!({
            "items": [
                { "meta": { "name": "a", "secret": "1" } },
                { "meta": { "name": "b", "secret": "2" } }
            ]
        }));
        strip(
            &mut r,
            &FieldDenial::Nested {
                array_key: "items".into(),
                array_block_type: None,
                row_path: vec![DenialSeg::Group("meta".into())],
                leaf: "secret".into(),
            },
        );
        assert_eq!(
            r["items"],
            json!([{ "meta": { "name": "a" } }, { "meta": { "name": "b" } }])
        );
    }

    #[test]
    fn nested_strips_array_inside_array() {
        let mut r = root(json!({
            "outer": [
                { "inner": [ { "secret": "1" }, { "secret": "2" } ] },
                { "inner": [ { "secret": "3" } ] }
            ]
        }));
        strip(
            &mut r,
            &FieldDenial::Nested {
                array_key: "outer".into(),
                array_block_type: None,
                row_path: vec![DenialSeg::Rows {
                    key: "inner".into(),
                    block_type: None,
                }],
                leaf: "secret".into(),
            },
        );
        assert_eq!(
            r["outer"],
            json!([{ "inner": [{}, {}] }, { "inner": [{}] }])
        );
    }

    #[test]
    fn nested_resolves_pre_hydration_group_prefixed_array_key() {
        // Group-nested array, write side: flat key `config__items`.
        let mut r = root(json!({
            "config__items": [ { "secret": "1" } ]
        }));
        strip(
            &mut r,
            &FieldDenial::Nested {
                array_key: "config__items".into(),
                array_block_type: None,
                row_path: vec![],
                leaf: "secret".into(),
            },
        );
        assert_eq!(r["config__items"], json!([{}]));
    }

    #[test]
    fn nested_resolves_post_hydration_group_nested_array_key() {
        // Group-nested array, read side: nested `config` -> `items`.
        let mut r = root(json!({
            "config": { "items": [ { "secret": "1", "keep": "k" } ] }
        }));
        strip(
            &mut r,
            &FieldDenial::Nested {
                array_key: "config__items".into(),
                array_block_type: None,
                row_path: vec![],
                leaf: "secret".into(),
            },
        );
        assert_eq!(r["config"], json!({ "items": [ { "keep": "k" } ] }));
    }

    #[test]
    fn blocks_denial_only_strips_its_own_block_type() {
        // Regression: a denial on `secret` in block type `premium` must NOT
        // strip the same-named field from a sibling `free` block that allows it.
        let mut r = root(json!({
            "content": [
                { "_block_type": "premium", "secret": "x", "keep": "p" },
                { "_block_type": "free", "secret": "ALLOWED", "keep": "f" }
            ]
        }));
        strip(
            &mut r,
            &FieldDenial::Nested {
                array_key: "content".into(),
                array_block_type: Some("premium".into()),
                row_path: vec![],
                leaf: "secret".into(),
            },
        );
        assert_eq!(
            r["content"],
            json!([
                { "_block_type": "premium", "keep": "p" },
                { "_block_type": "free", "secret": "ALLOWED", "keep": "f" }
            ]),
            "only the premium block's secret is stripped; free's is preserved"
        );
    }

    #[test]
    fn missing_keys_are_noops() {
        let mut r = root(json!({ "a": 1 }));
        strip(&mut r, &"nope".into());
        strip(
            &mut r,
            &FieldDenial::Nested {
                array_key: "missing".into(),
                array_block_type: None,
                row_path: vec![DenialSeg::Group("g".into())],
                leaf: "x".into(),
            },
        );
        assert_eq!(r["a"], json!(1));
    }

    #[test]
    fn from_str_and_string_build_flat() {
        assert_eq!(FieldDenial::from("x"), FieldDenial::Flat("x".into()));
        assert_eq!(
            FieldDenial::from("y".to_string()),
            FieldDenial::Flat("y".into())
        );
    }
}
