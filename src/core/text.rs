//! Canonical forms of stored text values.
//!
//! The same characters can be typed more than one way: an accent as one
//! precomposed code point or as a letter plus a combining mark, an address in
//! any mix of capitals. Stored values use one form so equality, uniqueness,
//! login and filters never depend on how a value was typed:
//!
//! - `Text` and `Textarea` values are NFC-composed (case is kept).
//! - `Email` values are trimmed, lowercased and NFC-composed.

use std::borrow::Cow;

use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::core::{
    FieldDefinition, FieldType, JsonRoot, VisitAction, normalize_email, walk_nested_mut,
};

/// NFC-compose `raw`, keeping its case.
#[must_use]
pub fn normalize_text(raw: &str) -> String {
    raw.nfc().collect()
}

/// The stored form of `raw` for a field of `field_type`, or `None` when the
/// type keeps its value as typed.
#[must_use]
pub fn canonical_text(field_type: &FieldType, raw: &str) -> Option<String> {
    match field_type {
        FieldType::Email => Some(normalize_email(raw)),
        FieldType::Text | FieldType::Textarea => Some(normalize_text(raw)),
        _ => None,
    }
}

/// A comparison operand in the form values of `field_type` are stored in, so a
/// filter matches however the operand was typed. Unchanged for a type without
/// a canonical form, or when the type is unknown.
#[must_use]
pub fn canonical_operand<'a>(field_type: Option<&FieldType>, raw: &'a str) -> Cow<'a, str> {
    field_type
        .and_then(|ft| canonical_text(ft, raw))
        .map_or(Cow::Borrowed(raw), Cow::Owned)
}

/// Whether values of `field_type` are stored in a canonical form.
#[must_use]
pub fn has_canonical_form(field_type: &FieldType) -> bool {
    matches!(
        field_type,
        FieldType::Email | FieldType::Text | FieldType::Textarea
    )
}

/// Rewrite every email and text value in `data` to its stored form, at any
/// depth: groups, array rows and blocks included. A localized value given as
/// an object of locale → value and a has-many text list are rewritten element
/// by element.
pub fn canonicalize_text_values<R>(data: &mut R, fields: &[FieldDefinition])
where
    R: JsonRoot,
{
    walk_nested_mut(data, fields, &mut Vec::new(), &mut |field, level, _| {
        level
            .root_get(&field.name)
            .and_then(|v| canonical_value(&field.field_type, v))
            .map_or(VisitAction::Keep, VisitAction::Replace)
    });
}

/// The stored form of a whole field value, or `None` when it is already
/// canonical (or the type keeps its value as typed).
fn canonical_value(field_type: &FieldType, value: &Value) -> Option<Value> {
    if !has_canonical_form(field_type) {
        return None;
    }

    let canonical = canonical_shape(field_type, value);

    (canonical != *value).then_some(canonical)
}

fn canonical_shape(field_type: &FieldType, value: &Value) -> Value {
    match value {
        Value::String(s) => {
            Value::String(canonical_text(field_type, s).unwrap_or_else(|| s.clone()))
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| canonical_shape(field_type, v))
                .collect(),
        ),
        Value::Object(by_locale) => Value::Object(
            by_locale
                .iter()
                .map(|(locale, v)| (locale.clone(), canonical_shape(field_type, v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;

    #[test]
    fn text_keeps_case_and_composes_accents() {
        assert_eq!(normalize_text("Cafe\u{301}"), "Caf\u{e9}");
        assert_eq!(
            canonical_text(&FieldType::Textarea, "Cafe\u{301}").as_deref(),
            Some("Caf\u{e9}")
        );
    }

    #[test]
    fn other_types_keep_their_value_as_typed() {
        for ft in [
            FieldType::Code,
            FieldType::Richtext,
            FieldType::Select,
            FieldType::Json,
        ] {
            assert_eq!(canonical_text(&ft, "Cafe\u{301}"), None, "{ft:?}");
        }
    }

    /// Localized values given per locale and has-many lists are rewritten
    /// element by element; a code field next to them is left alone.
    #[test]
    fn rewrites_locale_objects_and_lists() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
            FieldDefinition::builder("snippet", FieldType::Code).build(),
        ];

        let mut data: Map<String, Value> = json!({
            "title": { "en": "Cafe\u{301}", "de": "Cre\u{300}me" },
            "tags": ["Cafe\u{301}", "plain"],
            "snippet": "Cafe\u{301}",
        })
        .as_object()
        .unwrap()
        .clone();

        canonicalize_text_values(&mut data, &fields);

        assert_eq!(
            data["title"],
            json!({ "en": "Caf\u{e9}", "de": "Cr\u{e8}me" })
        );
        assert_eq!(data["tags"], json!(["Caf\u{e9}", "plain"]));
        assert_eq!(data["snippet"], json!("Cafe\u{301}"));
    }
}
