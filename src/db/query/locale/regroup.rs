//! All-locales read shaping: per-locale columns regrouped into `{ en, de }`
//! objects.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{Document, DocumentFields, FieldDefinition},
    db::query::helpers::{locale_column, prefixed_name, walk_leaf_fields},
};

/// Group locale-suffixed fields into nested objects for `LocaleMode::All`.
/// Converts `title__en: "Hello", title__de: "Hallo"` into `title: { en: "Hello", de: "Hallo" }`.
pub(crate) fn group_locale_fields(
    doc: &mut Document,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if !field.has_parent_column() {
                return Ok(());
            }

            let is_localized =
                (inherited_localized || field.localized) && locale_config.is_enabled();

            if !is_localized {
                return Ok(());
            }

            // A companion (`<name>_tz`, `<name>_lang`) is localized with its field.
            for column in field.columns_with_companions(&prefixed_name(prefix, &field.name)) {
                regroup_by_locale(&mut doc.fields, &column, locale_config)?;
            }

            Ok(())
        },
    )
}

/// Move the per-locale columns of `base` (`title__en`, `title__de`) into one
/// `{ en, de }` object under `base` — the shape an all-locales read returns,
/// for a stored row and a draft snapshot alike.
pub(crate) fn regroup_by_locale(
    fields: &mut DocumentFields,
    base: &str,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut locale_map = Map::new();

    for locale in &locale_config.locales {
        let col = locale_column(base, locale)?;

        if let Some(val) = fields.remove(&col) {
            locale_map.insert(locale.clone(), val);
        }
    }

    if !locale_map.is_empty() {
        fields.insert(base.to_string(), Value::Object(locale_map));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::FieldType,
        db::query::{
            locale::test_support::localized_code_lang_field,
            test_helpers::{make_group_field, make_locale_config, make_localized_field},
        },
    };

    #[test]
    fn group_locale_fields_basic() {
        let fields = vec![make_localized_field("title", FieldType::Text)];
        let locale_cfg = make_locale_config();
        let mut doc = Document::new("id1".to_string());
        doc.fields.insert("title__en".to_string(), json!("Hello"));
        doc.fields.insert("title__de".to_string(), json!("Hallo"));

        group_locale_fields(&mut doc, &fields, &locale_cfg).unwrap();

        let title = doc.fields.get("title").expect("title should exist");
        assert_eq!(title.get("en").and_then(|v| v.as_str()), Some("Hello"));
        assert_eq!(title.get("de").and_then(|v| v.as_str()), Some("Hallo"));
        assert!(!doc.fields.contains_key("title__en"));
        assert!(!doc.fields.contains_key("title__de"));
    }

    /// Regression: a localized timezone date read with every locale came back
    /// as a per-locale map while its `<name>_tz` companion stayed as flat
    /// `starts_tz__en` keys, which the generated types don't carry.
    #[test]
    fn group_locale_fields_regroups_a_timezone_companion() {
        let fields = vec![
            FieldDefinition::builder("starts", FieldType::Date)
                .timezone(true)
                .localized(true)
                .build(),
        ];
        let mut doc = Document::new("id1".to_string());
        for (key, value) in [
            ("starts__en", "2026-01-01T09:00:00.000Z"),
            ("starts__de", "2026-01-01T10:00:00.000Z"),
            ("starts_tz__en", "Europe/London"),
            ("starts_tz__de", "Europe/Berlin"),
        ] {
            doc.fields.insert(key.to_string(), json!(value));
        }

        group_locale_fields(&mut doc, &fields, &make_locale_config()).unwrap();

        assert_eq!(
            doc.fields.get("starts_tz"),
            Some(&json!({ "en": "Europe/London", "de": "Europe/Berlin" }))
        );
        assert!(!doc.fields.contains_key("starts_tz__en"));
    }

    #[test]
    fn group_locale_fields_with_group_prefix() {
        let fields = vec![make_group_field(
            "seo",
            vec![make_localized_field("title", FieldType::Text)],
        )];
        let locale_cfg = make_locale_config();
        let mut doc = Document::new("id1".to_string());
        doc.fields
            .insert("seo__title__en".to_string(), json!("SEO EN"));
        doc.fields
            .insert("seo__title__de".to_string(), json!("SEO DE"));

        group_locale_fields(&mut doc, &fields, &locale_cfg).unwrap();

        let seo_title = doc
            .fields
            .get("seo__title")
            .expect("seo__title should exist");
        assert_eq!(seo_title.get("en").and_then(|v| v.as_str()), Some("SEO EN"));
        assert_eq!(seo_title.get("de").and_then(|v| v.as_str()), Some("SEO DE"));
        assert!(!doc.fields.contains_key("seo__title__en"));
        assert!(!doc.fields.contains_key("seo__title__de"));
    }

    /// Regression: an all-locales read regrouped a localized code value into a
    /// per-locale map but left its `_lang` companion as flat per-locale keys.
    #[test]
    fn group_locale_fields_regroups_a_code_language_companion() {
        let fields = vec![localized_code_lang_field("snippet")];
        let mut doc = Document::new("id1".to_string());
        for (key, value) in [
            ("snippet__en", "fn main() {}"),
            ("snippet__de", "print()"),
            ("snippet_lang__en", "rust"),
            ("snippet_lang__de", "python"),
        ] {
            doc.fields.insert(key.to_string(), json!(value));
        }

        group_locale_fields(&mut doc, &fields, &make_locale_config()).unwrap();

        assert_eq!(
            doc.fields.get("snippet_lang"),
            Some(&json!({ "en": "rust", "de": "python" }))
        );
        assert!(!doc.fields.contains_key("snippet_lang__en"));
    }
}
