//! Leaf-shape and path helpers behind the write-access strips: which leaves
//! are localized or checkboxes, their companion columns, and the `__`-joined
//! path edits the update and snapshot strips apply.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::{
    core::{FieldDefinition, FieldType, prefixed_name, walk_leaf_fields},
    db::{
        LocaleContext,
        query::helpers::{column_belongs_to, locale_column},
    },
};

/// The locales a snapshot strip judges: the write's own locale for the shared
/// (non-localized) fields, and every configured locale for the localized ones,
/// since the snapshot carries a column per locale and each is published.
#[derive(Clone, Copy, Default)]
pub struct SnapshotLocales<'a> {
    /// The locale the write targets — what the request strip judges with.
    pub request: Option<&'a str>,
    /// Every configured locale; empty when localization is off, in which case
    /// every field is judged once, as a shared one.
    pub configured: &'a [String],
    /// The default locale, whose value a snapshot may also carry under the
    /// field's bare key.
    pub default: Option<&'a str>,
}

impl<'a> SnapshotLocales<'a> {
    /// The locales of a write running under `locale_ctx`.
    #[must_use]
    pub fn for_write(locale_ctx: Option<&'a LocaleContext>) -> Self {
        Self {
            request: locale_ctx.map(LocaleContext::access_locale),
            configured: locale_ctx.map_or(&[], |ctx| ctx.config.locales.as_slice()),
            default: locale_ctx.map(|ctx| ctx.config.default_locale.as_str()),
        }
    }
}

/// What the strip needs to know about the fields' leaves: which paths are
/// localized, which are checkboxes, and each leaf's companion columns.
pub(super) struct LeafShape {
    leaves: Vec<String>,
    pub(super) localized: HashSet<String>,
    checkboxes: HashSet<String>,
    companions: HashMap<String, Vec<String>>,
}

impl LeafShape {
    pub(super) fn of(fields: &[FieldDefinition], locales_enabled: bool) -> Self {
        let mut shape = Self {
            leaves: Vec::new(),
            localized: HashSet::new(),
            checkboxes: HashSet::new(),
            companions: HashMap::new(),
        };

        let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
            let path = prefixed_name(prefix, &field.name);

            if locales_enabled && (field.localized || inherited) {
                shape.localized.insert(path.clone());
            }
            if field.field_type == FieldType::Checkbox {
                shape.checkboxes.insert(path.clone());
            }
            shape
                .companions
                .insert(path.clone(), field.companion_columns(&path).collect());
            shape.leaves.push(path);

            Ok(())
        });

        shape
    }

    /// The leaf paths a removed path covers — itself, or every leaf under it
    /// when a whole group was removed.
    pub(super) fn leaves_under<'s>(
        &'s self,
        removed: &'s [String],
    ) -> impl Iterator<Item = &'s String> {
        self.leaves.iter().filter(move |leaf| {
            removed
                .iter()
                .any(|path| *leaf == path || leaf.starts_with(&format!("{path}__")))
        })
    }

    /// The column paths of `leaf` at `code`: the value and its companions —
    /// and, for the default locale, the bare key a snapshot may carry the
    /// default value under as well.
    pub(super) fn locale_columns(&self, leaf: &str, code: &str, is_default: bool) -> Vec<String> {
        let bases = std::iter::once(leaf.to_string())
            .chain(self.companions.get(leaf).into_iter().flatten().cloned());
        let mut columns: Vec<String> = bases
            .flat_map(|base| {
                let decorated = locale_column(&base, code).ok();
                is_default
                    .then(|| base.clone())
                    .into_iter()
                    .chain(decorated)
            })
            .collect();
        columns.sort();
        columns.dedup();

        columns
    }
}

/// The `__`-joined path of every key present in `before` but missing from
/// `after`.
pub(super) fn removed_paths(
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Vec<String> {
    let mut removed = Vec::new();
    collect_removed_paths(before, after, "", &mut removed);

    removed
}

/// Remove every key whose `__`-joined path is one of `paths`, at this level
/// and inside nested group objects.
pub(super) fn drop_paths(level: &mut Map<String, Value>, prefix: &str, paths: &[String]) {
    level.retain(|key, _| !paths.contains(&format!("{prefix}{key}")));

    for (key, value) in level.iter_mut() {
        if let Value::Object(nested) = value {
            drop_paths(nested, &format!("{prefix}{key}__"), paths);
        }
    }
}

/// The value stored at a `__`-joined path of a nested document.
fn value_at<'v>(level: &'v Map<String, Value>, path: &str) -> Option<&'v Value> {
    let (head, rest) = path.split_once("__").unwrap_or((path, ""));
    let value = level.get(head)?;

    if rest.is_empty() {
        return Some(value);
    }

    value_at(value.as_object()?, rest)
}

/// Insert `value` at a `__`-joined path, creating the group objects on the way.
fn insert_at(level: &mut Map<String, Value>, path: &str, value: Value) {
    let (head, rest) = path.split_once("__").unwrap_or((path, ""));

    if rest.is_empty() {
        level.insert(head.to_string(), value);
        return;
    }

    let nested = level
        .entry(head.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(nested) = nested {
        insert_at(nested, rest, value);
    }
}

/// Put every checkbox absent from `level` into it with its stored value, so
/// the strip judges its rule; returns the paths added.
///
/// A checkbox absent from a write is stored as unchecked, so a write-denied
/// checkbox the caller simply omitted would be flipped by the row write with
/// nothing for the strip to remove. Filled in, it is judged like a sent
/// value: denied, it keeps the stored value ([`restore_stripped_checkboxes`]);
/// allowed, it is taken out again ([`unfill_kept_checkboxes`]) so an omitted
/// checkbox still means "unchecked" for a caller who may write it.
pub(super) fn prefill_checkboxes(
    shape: &LeafShape,
    level: &mut Map<String, Value>,
    stored: &Map<String, Value>,
) -> Vec<String> {
    let mut filled = Vec::new();

    for leaf in &shape.leaves {
        if !shape.checkboxes.contains(leaf) || value_at(level, leaf).is_some() {
            continue;
        }

        if let Some(value) = value_at(stored, leaf) {
            insert_at(level, leaf, value.clone());
            filled.push(leaf.clone());
        }
    }

    filled
}

/// Take the pre-filled checkboxes the strip kept out again, along with any
/// group object the fill created that is empty now.
pub(super) fn unfill_kept_checkboxes(
    filled: &[String],
    removed: &[String],
    level: &mut Map<String, Value>,
    original: &Map<String, Value>,
) {
    let kept: Vec<String> = filled
        .iter()
        .filter(|leaf| {
            !removed
                .iter()
                .any(|path| *leaf == path || leaf.starts_with(&format!("{path}__")))
        })
        .cloned()
        .collect();
    drop_paths(level, "", &kept);

    level.retain(|key, value| {
        original.contains_key(key) || !value.as_object().is_some_and(Map::is_empty)
    });
}

/// Put the stored value back for every checkbox the strip removed.
///
/// A write-denied field is left untouched by dropping it from the data — for
/// every field but a checkbox, whose absence the row write reads as
/// "unchecked" and stores as `0`. Restoring the stored value keeps the row
/// write blind to the difference and the denied field genuinely untouched.
pub(super) fn restore_stripped_checkboxes(
    shape: &LeafShape,
    removed: &[String],
    level: &mut Map<String, Value>,
    stored: &Map<String, Value>,
) {
    for leaf in shape.leaves_under(removed) {
        if !shape.checkboxes.contains(leaf) {
            continue;
        }

        if let Some(value) = value_at(stored, leaf) {
            insert_at(level, leaf, value.clone());
        }
    }
}

/// Drop the columns that belong to every field a write strip removed from a
/// version snapshot: its per-locale columns (`price__en`, `seo__title__de`) and
/// a date's timezone companion (`starts_tz`, `starts_tz__de`).
///
/// Snapshots carry these beside a field's resolved value, and restore writes
/// them. The strip removes only the resolved key, so without this a
/// write-denied field would still be overwritten through its companions.
/// Handles companions kept at the top level and inside nested group objects
/// alike.
pub(super) fn drop_locale_columns_of_stripped(
    before: &Map<String, Value>,
    after: &mut Map<String, Value>,
) {
    let mut removed = Vec::new();
    collect_removed_paths(before, after, "", &mut removed);

    if removed.is_empty() {
        return;
    }

    drop_decorated(after, "", &removed);
}

/// Collect the `__`-joined path of every key present in `before` but missing
/// from `after`, descending into objects present in both.
fn collect_removed_paths(
    before: &Map<String, Value>,
    after: &Map<String, Value>,
    prefix: &str,
    removed: &mut Vec<String>,
) {
    for (key, value) in before {
        let path = format!("{prefix}{key}");

        match (value, after.get(key)) {
            (_, None) => removed.push(path),
            (Value::Object(b), Some(Value::Object(a))) => {
                collect_removed_paths(b, a, &format!("{path}__"), removed);
            }
            _ => {}
        }
    }
}

/// Remove every key whose `__`-joined path belongs to one of the `removed`
/// field paths, at this level and in nested objects.
fn drop_decorated(level: &mut Map<String, Value>, prefix: &str, removed: &[String]) {
    level.retain(|key, _| {
        let path = format!("{prefix}{key}");
        !removed.iter().any(|field| column_belongs_to(&path, field))
    });

    for (key, value) in level.iter_mut() {
        if let Value::Object(nested) = value {
            drop_decorated(nested, &format!("{prefix}{key}__"), removed);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Regression: a stripped field's companion columns were dropped for
    /// per-locale and timezone keys only, so a denied code field kept its
    /// `_lang` companion in the write.
    #[test]
    fn a_stripped_code_field_drops_its_language_companion() {
        let before = json!({ "snippet": "x", "snippet_lang": "rust", "title": "t" });
        let mut after = json!({ "snippet_lang": "rust", "title": "t" });

        drop_locale_columns_of_stripped(
            before.as_object().unwrap(),
            after.as_object_mut().unwrap(),
        );

        assert_eq!(after, json!({ "title": "t" }));
    }

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!("fixture is an object"),
        }
    }

    /// A stripped localized field must lose its decorated columns too, or a
    /// restore writes them back.
    #[test]
    fn stripped_fields_lose_their_locale_columns() {
        let before = object(json!({
            "price": 10, "price__en": 10, "price__de": 12,
            "title": "t", "title__en": "t",
            "starts": "2024-01-01T10:00", "starts_tz": "Europe/Berlin",
            "starts_tz__de": "Europe/Berlin", "starts_at_home": "kept",
            "seo": { "title": "x", "title__en": "x", "desc": "d" },
            "seo__title__de": "y",
        }));
        let mut after = before.clone();
        after.remove("price");
        after.remove("starts");
        after["seo"].as_object_mut().unwrap().remove("title");

        drop_locale_columns_of_stripped(&before, &mut after);

        assert_eq!(
            Value::Object(after),
            json!({
                "title": "t", "title__en": "t",
                "starts_at_home": "kept",
                "seo": { "desc": "d" },
            })
        );
    }
}
