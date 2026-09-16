//! Per-leaf SELECT expressions: a stored column and its companions, expanded
//! to the reading locale's column (with fallback) or every locale's column.

use anyhow::Result;

use crate::{
    core::FieldDefinition,
    db::{
        LocaleContext,
        query::helpers::{locale_column, prefixed_name, quote_ident, walk_leaf_fields},
    },
};

/// Push a column (or its locale-expanded variants) onto the SELECT lists.
fn push_column(
    select_exprs: &mut Vec<String>,
    result_names: &mut Vec<String>,
    col: &str,
    is_localized: bool,
    locale_ctx: &LocaleContext,
) -> Result<()> {
    if is_localized {
        add_locale_columns(select_exprs, result_names, col, locale_ctx)
    } else {
        // SELECT identifier quoted (reserved-word-safe); bare name for mapping.
        select_exprs.push(quote_ident(col));
        result_names.push(col.to_string());
        Ok(())
    }
}

/// Collect locale-aware SELECT columns from a field tree using `walk_leaf_fields`.
pub(super) fn collect_locale_columns(
    fields: &[FieldDefinition],
    select_exprs: &mut Vec<String>,
    result_names: &mut Vec<String>,
    locale_ctx: &LocaleContext,
) -> Result<()> {
    walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if !field.has_parent_column() {
                return Ok(());
            }

            let base = prefixed_name(prefix, &field.name);
            let is_localized =
                (inherited_localized || field.localized) && locale_ctx.config.is_enabled();

            // A companion column is localized with its field.
            for column in field.columns_with_companions(&base) {
                push_column(
                    select_exprs,
                    result_names,
                    &column,
                    is_localized,
                    locale_ctx,
                )?;
            }

            Ok(())
        },
    )
}

/// Add SELECT expressions for a localized field: the reading locale's column
/// (falling back to the default locale's while it is NULL), or every locale's
/// column for an all-locales read.
fn add_locale_columns(
    select_exprs: &mut Vec<String>,
    result_names: &mut Vec<String>,
    field_name: &str,
    locale_ctx: &LocaleContext,
) -> Result<()> {
    let Some(read) = locale_ctx.read_locale() else {
        for locale in &locale_ctx.config.locales {
            let col = locale_column(field_name, locale)?;
            select_exprs.push(quote_ident(&col));
            result_names.push(col);
        }

        return Ok(());
    };

    // The one read expression — shared with the filter, the sort and the
    // keyset, so a value the SELECT shows is the value they compare against.
    let value = read.column_expr(field_name)?;
    let alias = quote_ident(field_name);

    select_exprs.push(format!("{value} AS {alias}"));
    result_names.push(field_name.to_string());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{FieldTab, FieldType},
        db::{
            LocaleMode,
            query::{
                column_read_expr, get_locale_select_columns,
                locale::test_support::localized_code_lang_field,
                test_helpers::{
                    make_field, make_group_field, make_locale_config, make_localized_field,
                    make_row_field, make_tabs_field,
                },
            },
        },
    };

    /// The SELECT's source expression for a localized column IS the expression
    /// the filter, the sort and the keyset read it through. If the two ever
    /// drift, a listed fallback value stops matching its own filter and the
    /// page boundaries stop lining up with the rows.
    #[test]
    fn the_select_reads_a_localized_column_through_the_shared_expression() {
        let fields = vec![make_localized_field("title", FieldType::Text)];

        for mode in [LocaleMode::Default, LocaleMode::Single("de".to_string())] {
            let ctx = LocaleContext {
                mode,
                config: make_locale_config(),
            };

            let (exprs, _) = get_locale_select_columns(&fields, false, &ctx).unwrap();
            let shared = column_read_expr("title", &fields, Some(&ctx)).unwrap();

            assert!(
                exprs.contains(&format!("{shared} AS \"title\"")),
                "select must read `title` through `{shared}`, got: {exprs:?}"
            );
        }
    }

    #[test]
    fn get_locale_select_columns_tabs_with_group() {
        let fields = vec![make_tabs_field(
            "layout",
            vec![FieldTab::new(
                "Social",
                vec![make_group_field(
                    "social",
                    vec![make_field("github", FieldType::Text)],
                )],
            )],
        )];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();
        assert!(
            exprs.contains(&"\"social__github\"".to_string()),
            "Group inside Tabs should appear in SELECT"
        );
        assert!(names.contains(&"social__github".to_string()));
    }

    #[test]
    fn get_locale_select_columns_tabs_with_localized_field() {
        let fields = vec![make_tabs_field(
            "layout",
            vec![FieldTab::new(
                "Content",
                vec![make_localized_field("title", FieldType::Text)],
            )],
        )];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();
        assert!(
            exprs.iter().any(|e| e.contains("title__de")),
            "Localized field in Tabs should have locale column"
        );
        assert!(names.contains(&"title".to_string()));
    }

    #[test]
    fn get_locale_select_columns_group_containing_tabs_localized() {
        let fields = vec![{
            let mut g = make_group_field(
                "meta",
                vec![make_tabs_field(
                    "t",
                    vec![FieldTab::new(
                        "Content",
                        vec![make_field("title", FieldType::Text)],
                    )],
                )],
            );
            g.localized = true;
            g
        }];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();
        assert!(
            exprs.iter().any(|e| e.contains("meta__title__de")),
            "Localized Group→Tabs: meta__title__de"
        );
        assert!(names.contains(&"meta__title".to_string()));
    }

    // ── Timezone companion column tests ──────────────────────────────

    #[test]
    fn get_locale_select_columns_includes_date_tz_in_row() {
        // Date field with timezone: true inside a Row should produce both
        // start_date and start_date_tz in the SELECT columns.
        let fields = vec![make_row_field(
            "r",
            vec![
                FieldDefinition::builder("start_date", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        )];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };

        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();

        assert!(
            exprs.contains(&"\"start_date\"".to_string()),
            "SELECT should include start_date, got: {exprs:?}"
        );
        assert!(
            exprs.contains(&"\"start_date_tz\"".to_string()),
            "SELECT should include start_date_tz, got: {exprs:?}"
        );
        assert!(
            names.contains(&"start_date".to_string()),
            "Result names should include start_date"
        );
        assert!(
            names.contains(&"start_date_tz".to_string()),
            "Result names should include start_date_tz"
        );
    }

    #[test]
    fn get_locale_select_columns_date_tz_non_localized_default_mode() {
        // Non-localized date field with timezone in Default locale mode:
        // should produce plain start_date and start_date_tz columns.
        let fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };

        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();

        assert_eq!(
            exprs,
            vec!["id", "\"start_date\"", "\"start_date_tz\""],
            "Non-localized date+tz should appear as plain columns"
        );
        assert_eq!(names, vec!["id", "start_date", "start_date_tz"]);
    }

    #[test]
    fn get_locale_select_columns_date_tz_localized_single_mode() {
        // Localized date field with timezone in Single locale mode:
        // date column gets locale suffix, _tz column also gets locale handling.
        let fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .localized(true)
                .build(),
        ];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };

        let (exprs, _names) = get_locale_select_columns(&fields, false, &ctx).unwrap();

        // The date column should have locale handling (COALESCE for fallback)
        assert!(
            exprs.iter().any(|e| e.contains("start_date__de")),
            "Localized date should include locale-suffixed column, got: {exprs:?}"
        );
        // The _tz column should also have locale handling
        assert!(
            exprs.iter().any(|e| e.contains("start_date_tz__de")),
            "Localized _tz should include locale-suffixed column, got: {exprs:?}"
        );
    }

    #[test]
    fn get_locale_select_columns_date_without_tz_no_companion() {
        // Date field WITHOUT timezone: true should NOT produce a _tz column.
        let fields = vec![FieldDefinition::builder("event_date", FieldType::Date).build()];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };

        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();

        assert_eq!(exprs, vec!["id", "\"event_date\""]);
        assert_eq!(names, vec!["id", "event_date"]);
        assert!(
            !exprs.iter().any(|e| e.contains("_tz")),
            "Date without timezone should not have _tz column"
        );
    }

    #[test]
    fn get_locale_select_columns_group_date_tz() {
        // Date field with timezone inside a Group should produce
        // group__field and group__field_tz columns.
        let fields = vec![make_group_field(
            "schedule",
            vec![
                FieldDefinition::builder("start", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        )];
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };

        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();

        assert!(
            exprs.contains(&"\"schedule__start\"".to_string()),
            "Group date should be prefixed: {exprs:?}"
        );
        assert!(
            exprs.contains(&"\"schedule__start_tz\"".to_string()),
            "Group date _tz should be prefixed: {exprs:?}"
        );
        assert!(names.contains(&"schedule__start".to_string()));
        assert!(names.contains(&"schedule__start_tz".to_string()));
    }

    // ── Code language companion column tests ─────────────────────────

    /// Regression: a localized code field's `_lang` companion was never
    /// selected, so a single-locale read never returned the language pick.
    #[test]
    fn get_locale_select_columns_includes_code_lang_localized_single_mode() {
        let fields = vec![localized_code_lang_field("snippet")];
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: make_locale_config(),
        };

        let (exprs, names) = get_locale_select_columns(&fields, false, &ctx).unwrap();

        assert!(
            exprs.contains(
                &"COALESCE(\"snippet_lang__de\", \"snippet_lang__en\") AS \"snippet_lang\""
                    .to_string()
            ),
            "Localized _lang should read the locale column with fallback, got: {exprs:?}"
        );
        assert!(names.contains(&"snippet_lang".to_string()));
    }
}
