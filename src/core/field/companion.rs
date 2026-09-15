//! Companion columns: the text a field stores beside its own column — a
//! timezone date's zone (`_tz`), a code field's language pick (`_lang`).
//!
//! A companion is part of its field's value: it is selected, written, localized,
//! snapshotted, restored and nested into a group together with it. Every one of
//! those paths iterates the list below, so a companion can't be handled by some
//! and missed by others.

use std::iter;

use crate::core::{FieldDefinition, FieldType};

/// Suffix of a Date field's timezone companion column. The single source of
/// truth shared by column generation (`query::helpers::tz_column`) and
/// field-name reservation (`parse::fields` rejects user fields ending in this)
/// so the two can't drift.
pub(crate) const TZ_SUFFIX: &str = "_tz";

/// Suffix of a Code field's language companion column. Single source of truth
/// shared by `query::helpers::lang_column` and field-name reservation — see
/// [`TZ_SUFFIX`].
pub(crate) const LANG_SUFFIX: &str = "_lang";

/// When a write stores a companion column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompanionWrite {
    /// Written whenever the field's value is: the companion gives the value its
    /// meaning, so the two can never disagree.
    WithValue,
    /// Written only when its own key is sent, so a write that leaves it out
    /// keeps the stored one.
    WhenSent,
}

/// One companion a field stores beside its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Companion {
    /// Suffix appended to the field's column name (`_tz`, `_lang`).
    pub suffix: &'static str,
    /// When a write stores the companion.
    pub write: CompanionWrite,
    /// What the companion holds, as clients are told — the one wording every
    /// generated schema shows, so no surface has to recognize the suffix.
    pub description: &'static str,
}

impl FieldDefinition {
    /// Whether the field stores a timezone companion (`{name}_tz`) beside its
    /// value — a Date with `timezone` enabled.
    #[must_use]
    pub fn has_tz_companion(&self) -> bool {
        self.field_type == FieldType::Date && self.timezone
    }

    /// Whether the field stores a language companion (`{name}_lang`) beside its
    /// value — a Code field with a non-empty `admin.languages` allow-list, whose
    /// companion holds the editor's per-document language pick.
    #[must_use]
    pub fn has_lang_companion(&self) -> bool {
        self.field_type == FieldType::Code && !self.admin.languages.is_empty()
    }

    /// Each companion the field stores: its suffix, when a write stores it, and
    /// what it holds. The one table every companion-aware surface reads, so a
    /// surface can't have to recognize a suffix of its own.
    pub(crate) fn companion_descriptors(&self) -> impl Iterator<Item = Companion> {
        [
            (
                self.has_tz_companion(),
                Companion {
                    suffix: TZ_SUFFIX,
                    write: CompanionWrite::WithValue,
                    description: "IANA timezone of the date",
                },
            ),
            (
                self.has_lang_companion(),
                Companion {
                    suffix: LANG_SUFFIX,
                    write: CompanionWrite::WhenSent,
                    description: "Language the code is written in (one of the field's languages)",
                },
            ),
        ]
        .into_iter()
        .filter_map(|(stored, companion)| stored.then_some(companion))
    }

    /// The suffixes of the companion columns the field stores beside its value.
    pub fn companion_suffixes(&self) -> impl Iterator<Item = &'static str> {
        self.companion_descriptors().map(|c| c.suffix)
    }

    /// The companion columns a write of the field's column `base` stores. A
    /// companion bound to its value — a date's zone, which gives the date its
    /// meaning — is written whenever the value is (`value_sent`). Any other — a
    /// code field's language pick — is written only when its own key is sent
    /// (`is_sent`), so a write that leaves it out keeps the stored one.
    pub fn written_companion_columns<F: Fn(&str) -> bool>(
        &self,
        base: &str,
        value_sent: bool,
        is_sent: F,
    ) -> impl Iterator<Item = String> {
        self.companion_descriptors().filter_map(move |companion| {
            let column = format!("{base}{}", companion.suffix);

            let written = match companion.write {
                CompanionWrite::WithValue => value_sent,
                CompanionWrite::WhenSent => is_sent(&column),
            };

            written.then_some(column)
        })
    }

    /// The companion columns of the field's column `base`: `starts_tz`,
    /// `meta__example_lang`.
    pub fn companion_columns(&self, base: &str) -> impl Iterator<Item = String> {
        self.companion_suffixes()
            .map(move |suffix| format!("{base}{suffix}"))
    }

    /// The field's column `base` followed by its companion columns — every
    /// column the field stores on its row, in the order reads select them.
    pub fn columns_with_companions(&self, base: &str) -> impl Iterator<Item = String> {
        iter::once(base.to_string()).chain(self.companion_columns(base))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldAdmin;

    fn code(languages: Vec<String>) -> FieldDefinition {
        FieldDefinition::builder("snippet", FieldType::Code)
            .admin(FieldAdmin::builder().languages(languages).build())
            .build()
    }

    fn suffixes(field: &FieldDefinition) -> Vec<&'static str> {
        field.companion_suffixes().collect()
    }

    #[test]
    fn a_timezone_date_has_a_tz_companion() {
        let zoned = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();
        assert_eq!(suffixes(&zoned), vec![TZ_SUFFIX]);

        let plain = FieldDefinition::builder("starts", FieldType::Date).build();
        assert!(suffixes(&plain).is_empty());
    }

    #[test]
    fn a_code_field_with_languages_has_a_lang_companion() {
        assert_eq!(
            suffixes(&code(vec!["python".to_string()])),
            vec![LANG_SUFFIX]
        );
        assert!(
            suffixes(&code(Vec::new())).is_empty(),
            "no companion without an allow-list"
        );
    }

    /// A zone is written with its date whether or not it was sent; a language
    /// pick is written only when its own key is sent.
    #[test]
    fn a_zone_is_written_with_its_value_a_language_only_when_sent() {
        let date = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();

        let with_date: Vec<String> = date
            .written_companion_columns("starts", true, |_| false)
            .collect();
        assert_eq!(with_date, vec!["starts_tz"]);
        assert_eq!(
            date.written_companion_columns("starts", false, |_| true)
                .count(),
            0
        );

        let code = code(vec!["python".to_string()]);
        assert_eq!(
            code.written_companion_columns("snippet", true, |_| false)
                .count(),
            0,
            "an absent language keeps the stored pick"
        );

        let sent: Vec<String> = code
            .written_companion_columns("snippet", false, |column| column == "snippet_lang")
            .collect();
        assert_eq!(sent, vec!["snippet_lang"]);
    }

    /// Each companion describes itself — suffix, write policy and the wording
    /// clients are shown — so a surface never has to recognize `_tz` / `_lang`.
    #[test]
    fn a_companion_describes_its_suffix_write_policy_and_value() {
        let date = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();

        assert_eq!(
            date.companion_descriptors().collect::<Vec<_>>(),
            vec![Companion {
                suffix: TZ_SUFFIX,
                write: CompanionWrite::WithValue,
                description: "IANA timezone of the date",
            }]
        );

        assert_eq!(
            code(vec!["python".to_string()])
                .companion_descriptors()
                .collect::<Vec<_>>(),
            vec![Companion {
                suffix: LANG_SUFFIX,
                write: CompanionWrite::WhenSent,
                description: "Language the code is written in (one of the field's languages)",
            }]
        );

        assert_eq!(
            FieldDefinition::builder("title", FieldType::Text)
                .build()
                .companion_descriptors()
                .count(),
            0
        );
    }

    #[test]
    fn other_fields_have_no_companion() {
        let text = FieldDefinition::builder("title", FieldType::Text)
            .timezone(true)
            .build();

        assert!(suffixes(&text).is_empty());
    }

    #[test]
    fn companion_columns_suffix_the_column() {
        let field = code(vec!["python".to_string()]);

        let companions: Vec<String> = field.companion_columns("meta__example").collect();
        assert_eq!(companions, vec!["meta__example_lang"]);

        let all: Vec<String> = field.columns_with_companions("meta__example").collect();
        assert_eq!(all, vec!["meta__example", "meta__example_lang"]);
    }
}
