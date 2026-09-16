//! A stored snapshot as the base of a write.
//!
//! Restore writes a whole snapshot back over a document, every locale at once,
//! as SQL. Publishing a pending draft needs the same content for ONE locale and
//! as data, so the fields the publishing request does not send come from the
//! draft. Both go through the same per-locale resolver ([`LocaleSnapshot`]), so
//! what a draft publishes is what a restore of that snapshot would write.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{DocumentFields, FieldDefinition},
    db::{
        LocaleContext,
        query::{
            helpers::{prefixed_name, walk_leaf_fields},
            locale_locked_field_names,
            versions::restore::locale_snapshot::{
                LocaleSnapshot, SnapshotKey, resolve_snapshot_value,
            },
        },
    },
};

/// A snapshot read for the single locale a write targets.
struct WriteBase<'a> {
    obj: &'a Map<String, Value>,
    /// The per-locale view and the locale the write stores into, while
    /// localization is on. `None` reads every key bare.
    per_locale: Option<(LocaleSnapshot<'a>, &'a str)>,
}

impl<'a> WriteBase<'a> {
    fn new(obj: &'a Map<String, Value>, locale_ctx: Option<&'a LocaleContext>) -> Self {
        let per_locale = locale_ctx
            .filter(|ctx| ctx.config.is_enabled())
            .map(|ctx| (LocaleSnapshot::new(obj, &ctx.config), ctx.access_locale()));

        Self { obj, per_locale }
    }

    /// The value this write takes for one column, or `None` where the snapshot
    /// carries none: a field added to the schema after the snapshot was taken,
    /// or a translation the draft never wrote. A missing value is left absent
    /// rather than filled from another locale — a write stores what it is given
    /// as the target locale's own value, so a fallback would turn the default
    /// locale's text into a translation.
    fn value(&self, key: SnapshotKey<'_>) -> Result<Option<&'a Value>> {
        let Some((snapshot, locale)) = &self.per_locale else {
            return Ok(resolve_snapshot_value(self.obj, key.0, key.1, key.2));
        };

        snapshot.value(key, locale)
    }
}

/// The values `snapshot` contributes to a write under `locale_ctx`, keyed the
/// way write data is keyed: flat `group__sub` columns, a join field (array,
/// blocks, has-many) under its own name, each companion (`_tz`, `_lang`) beside
/// its value.
///
/// Shared (non-localized) columns are dropped for a non-default single-locale
/// write — the same set [`locale_locked_field_names`] locks out of every other
/// write path, because such a write may not touch the canonical value at all.
///
/// Only schema fields are walked, so the document's server-managed columns
/// (`id`, `_status`, `_ref_count`, the timestamps) are never part of the base.
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
pub fn snapshot_write_fields(
    snapshot: &Map<String, Value>,
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> Result<DocumentFields> {
    let base = WriteBase::new(snapshot, locale_ctx);
    let mut out = DocumentFields::new();

    walk_leaf_fields(fields, "", false, &mut |field, prefix, _inherited| {
        let flat = prefixed_name(prefix, &field.name);

        // The value and its companions travel together: a snapshot stores each
        // under the same prefix, flat or nested inside its group object.
        let columns = field.columns_with_companions(&flat);
        let names = field.columns_with_companions(&field.name);

        for (column, name) in columns.zip(names) {
            let Some(value) = base.value((column.as_str(), prefix, name.as_str()))? else {
                continue;
            };

            out.insert(column, value.clone());
        }

        Ok(())
    })?;

    let locked = locale_locked_field_names(fields, locale_ctx);
    out.retain(|column, _| !locked.contains(column));

    Ok(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{FieldType, RelationshipConfig},
        db::LocaleMode,
    };

    fn fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("heading", FieldType::Text)
                        .localized(true)
                        .build(),
                    FieldDefinition::builder("robots", FieldType::Text).build(),
                ])
                .build(),
            FieldDefinition::builder("slides", FieldType::Array)
                .localized(true)
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ]
    }

    fn en_de(locale: &str) -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single(locale.to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        }
    }

    fn snapshot() -> Map<String, Value> {
        json!({
            "id": "p1",
            "_status": "draft",
            "_ref_count": 3,
            "created_at": "2026-01-01T00:00:00.000Z",
            "title": "English",
            "title__en": "English",
            "title__de": "Deutsch",
            "slug": "shared-slug",
            "seo": { "heading": "Head", "robots": "index" },
            "seo__heading__en": "Head",
            "seo__heading__de": "Kopf",
            "slides": [{ "id": "r1" }],
            "slides__en": [{ "id": "r1" }],
            "slides__de": [{ "id": "r2" }],
            "tags": ["t1"],
        })
        .as_object()
        .expect("object")
        .clone()
    }

    /// A default-locale publish takes that locale's own values, the shared
    /// columns, group sub-fields under their flat column, and both join fields.
    /// Server-managed columns are not part of a write's base.
    #[test]
    fn a_default_locale_base_carries_every_field_and_no_system_column() {
        let base = snapshot_write_fields(&snapshot(), &fields(), Some(&en_de("en"))).unwrap();

        assert_eq!(base.get("title"), Some(&json!("English")));
        assert_eq!(base.get("slug"), Some(&json!("shared-slug")));
        assert_eq!(base.get("seo__heading"), Some(&json!("Head")));
        assert_eq!(base.get("seo__robots"), Some(&json!("index")));
        assert_eq!(base.get("slides"), Some(&json!([{ "id": "r1" }])));
        assert_eq!(base.get("tags"), Some(&json!(["t1"])));

        for system in ["id", "_status", "_ref_count", "created_at"] {
            assert!(
                !base.contains_key(system),
                "a write never restates {system}: {base:?}"
            );
        }
    }

    /// A non-default-locale publish takes that locale's translations and
    /// nothing else: a shared column belongs to the default locale's row, and
    /// this write may not touch it — the same rule every other write path
    /// enforces.
    #[test]
    fn a_non_default_locale_base_is_that_locales_values_only() {
        let base = snapshot_write_fields(&snapshot(), &fields(), Some(&en_de("de"))).unwrap();

        assert_eq!(base.get("title"), Some(&json!("Deutsch")));
        assert_eq!(base.get("seo__heading"), Some(&json!("Kopf")));
        assert_eq!(base.get("slides"), Some(&json!([{ "id": "r2" }])));

        assert!(!base.contains_key("slug"), "{base:?}");
        assert!(!base.contains_key("seo__robots"), "{base:?}");
        assert!(
            !base.contains_key("tags"),
            "a shared join field is locked too: {base:?}"
        );
    }

    /// A translation the draft never wrote stays absent rather than picking up
    /// the default locale's text, which the write would store as that locale's
    /// own value.
    #[test]
    fn a_locale_the_snapshot_has_no_value_for_contributes_nothing() {
        let mut snapshot = snapshot();
        snapshot.remove("title__de");
        snapshot.remove("seo__heading__de");

        let base = snapshot_write_fields(&snapshot, &fields(), Some(&en_de("de"))).unwrap();

        assert!(!base.contains_key("title"), "{base:?}");
        assert!(!base.contains_key("seo__heading"), "{base:?}");
    }

    /// Without localization every key is read bare, and a companion column
    /// travels with the value it belongs to.
    #[test]
    fn without_locales_the_base_is_the_bare_keys_and_their_companions() {
        let fields = vec![
            FieldDefinition::builder("starts", FieldType::Date)
                .timezone(true)
                .build(),
        ];
        let snapshot =
            json!({ "starts": "2026-01-01T10:00:00.000Z", "starts_tz": "Europe/Berlin" })
                .as_object()
                .expect("object")
                .clone();

        let base = snapshot_write_fields(&snapshot, &fields, None).unwrap();

        assert_eq!(base.get("starts"), Some(&json!("2026-01-01T10:00:00.000Z")));
        assert_eq!(base.get("starts_tz"), Some(&json!("Europe/Berlin")));
    }
}
