//! Write-side locale decisions: the column a write targets, and which shared
//! fields a non-default-locale write must not touch.

use std::collections::HashSet;

use anyhow::Result;

use crate::{
    core::FieldDefinition,
    db::{
        LocaleContext, LocaleMode,
        query::helpers::{locale_column, prefixed_name, walk_leaf_fields},
    },
};

/// Map a flat field name to the actual locale-suffixed column name for writes.
///
/// `inherited_localized` accounts for parent Group localization: when a Group
/// has `localized: true`, all its children get locale-suffixed columns even if
/// the child's own `localized` flag is false.
pub(crate) fn locale_write_column(
    field_name: &str,
    field: &FieldDefinition,
    locale_ctx: Option<&LocaleContext>,
    inherited_localized: bool,
) -> Result<String> {
    let Some(ctx) = locale_ctx.filter(|c| c.config.is_enabled()) else {
        return Ok(field_name.to_string());
    };

    if !field.localized && !inherited_localized {
        return Ok(field_name.to_string());
    }

    locale_column(field_name, ctx.access_locale())
}

/// Whether a leaf field is "locale-locked" for a write: a shared
/// (locale-independent) field being written while editing a **non-default**
/// locale. Such a field maps to the bare column ([`locale_write_column`]), so
/// writing it under a non-default locale would clobber the canonical value that
/// belongs to the default locale.
///
/// The admin UI marks these fields read-only (`locale_locked`); this enforces
/// the same rule server-side, for every write surface (form, Lua, gRPC, MCP).
/// `inherited_localized` accounts for a localized parent Group, mirroring
/// [`locale_write_column`].
#[must_use]
pub(crate) fn is_locale_locked_write(
    field: &FieldDefinition,
    locale_ctx: Option<&LocaleContext>,
    inherited_localized: bool,
) -> bool {
    // A localized field (directly or via a localized Group) writes its own
    // per-locale column — never locked.
    if field.localized || inherited_localized {
        return false;
    }

    is_non_default_single_locale(locale_ctx)
}

/// Whether the write targets a single, **non-default** locale — the only mode in
/// which a shared (locale-independent) field must not be written, because it
/// would clobber the default-locale canonical value. `Default` / `All` (and an
/// explicit `Single(default)`) write shared columns as normal; localization
/// disabled is never locked.
#[must_use]
pub(crate) fn is_non_default_single_locale(locale_ctx: Option<&LocaleContext>) -> bool {
    let Some(ctx) = locale_ctx.filter(|c| c.config.is_enabled()) else {
        return false;
    };

    matches!(ctx.mode, LocaleMode::Single(_)) && ctx.access_locale() != ctx.config.default_locale
}

/// The set of flat field names (`seo__title`) that are locale-locked for a write
/// under `locale_ctx` — shared (non-localized, non-inherited) leaf fields when
/// the target is a non-default single locale. Empty otherwise. Used by write
/// paths that operate on flat data maps rather than per-field (draft snapshots).
#[must_use]
pub(crate) fn locale_locked_field_names(
    fields: &[FieldDefinition],
    locale_ctx: Option<&LocaleContext>,
) -> HashSet<String> {
    let mut locked = HashSet::new();
    if !is_non_default_single_locale(locale_ctx) {
        return locked;
    }

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, inherited| {
        // Classify the field the SAME way its live write path does, so the
        // draft snapshot's drop-set matches exactly:
        // - scalar column (`has_parent_column`): inheritance-aware, like
        //   `collect_leaf_update` / `is_locale_locked_write`.
        // - join-backed (array / blocks / has-many): own-flag-only, like
        //   `save_join_data_inner` (`resolve_join_locale` ignores a localized
        //   parent Group, so a shared join field is never per-locale).
        let field_locked = if field.has_parent_column() {
            is_locale_locked_write(field, locale_ctx, inherited)
        } else {
            !field.localized
        };

        if field_locked {
            let name = prefixed_name(prefix, &field.name);

            // A scalar's companion columns (`{field}_tz`, `{field}_lang`) are
            // part of its value. The live write skips the whole field under a
            // non-default locale, so the snapshot drop-set must drop the
            // companions too — otherwise a non-default-locale edit of one
            // survives into the snapshot and clobbers the canonical value on
            // restore.
            if field.has_parent_column() {
                locked.extend(field.companion_columns(&name));
            }

            locked.insert(name);
        }
        Ok(())
    });

    locked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::LocaleConfig,
        core::FieldType,
        db::query::{
            locale::test_support::localized_code_lang_field,
            test_helpers::{make_field, make_locale_config, make_localized_field},
        },
    };

    /// Regression: the shared-field lock checked the requested locale while the
    /// write went to the locale actually targeted, so a write under a locale
    /// that isn't configured — written to the default locale's columns — still
    /// locked shared fields.
    #[test]
    fn a_locale_that_is_not_configured_does_not_lock_shared_fields() {
        let ctx = LocaleContext {
            mode: LocaleMode::Single("fr".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        };

        assert!(!is_non_default_single_locale(Some(&ctx)));

        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            ..ctx
        };
        assert!(is_non_default_single_locale(Some(&de)));
    }

    #[test]
    fn locale_write_column_non_localized_passthrough() {
        let field = make_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, ctx_ref, false).unwrap();
        assert_eq!(
            col, "title",
            "Non-localized field should pass through unchanged"
        );
    }

    #[test]
    fn locale_write_column_localized_single() {
        let field = make_localized_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, ctx_ref, false).unwrap();
        assert_eq!(col, "title__de");
    }

    #[test]
    fn locale_write_column_localized_default_mode() {
        let field = make_localized_field("title", FieldType::Text);
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Default,
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);
        let col = locale_write_column("title", &field, ctx_ref, false).unwrap();
        assert_eq!(col, "title__en", "Default mode should use default locale");
    }

    // ── inherited_localized regression tests ──────────────────────────

    /// Regression: when a parent Group has `localized: true`, child fields
    /// with `localized: false` must still get a locale suffix via
    /// `inherited_localized`.
    #[test]
    fn locale_write_column_inherited_localized_adds_suffix() {
        let field = make_field("title", FieldType::Text); // localized = false
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);

        let col = locale_write_column("title", &field, ctx_ref, true).unwrap();
        assert_eq!(
            col, "title__de",
            "inherited_localized=true should add locale suffix even when field.localized=false"
        );
    }

    /// Regression: when `inherited_localized` is false and the field itself
    /// is not localized, the column name must be returned unchanged.
    #[test]
    fn locale_write_column_not_inherited_not_localized_unchanged() {
        let field = make_field("title", FieldType::Text); // localized = false
        let locale_cfg = make_locale_config();
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locale_cfg,
        };
        let ctx_ref: Option<&LocaleContext> = Some(&ctx);

        let col = locale_write_column("title", &field, ctx_ref, false).unwrap();
        assert_eq!(
            col, "title",
            "inherited_localized=false + field.localized=false should not add locale suffix"
        );
    }

    #[test]
    fn locale_locked_write_shared_field_non_default_locale() {
        // A shared (non-localized) field edited under a non-default locale is
        // locked — writing it would clobber the canonical default-locale value.
        let field = make_field("title", FieldType::Text);
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: make_locale_config(),
        };
        assert!(is_locale_locked_write(&field, Some(&ctx), false));
    }

    #[test]
    fn locale_locked_write_not_locked_for_default_or_all() {
        let field = make_field("title", FieldType::Text);
        // Explicit default locale writes shared columns.
        let single_default = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        assert!(!is_locale_locked_write(
            &field,
            Some(&single_default),
            false
        ));
        // Default / All modes too.
        let default_mode = LocaleContext {
            mode: LocaleMode::Default,
            config: make_locale_config(),
        };
        assert!(!is_locale_locked_write(&field, Some(&default_mode), false));
    }

    #[test]
    fn locale_locked_write_localized_field_never_locked() {
        let ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: make_locale_config(),
        };
        // A localized field writes its own per-locale column.
        let localized = make_localized_field("title", FieldType::Text);
        assert!(!is_locale_locked_write(&localized, Some(&ctx), false));
        // A shared field under a localized Group (inherited_localized) is kept.
        let shared = make_field("title", FieldType::Text);
        assert!(!is_locale_locked_write(&shared, Some(&ctx), true));
    }

    #[test]
    fn locale_locked_write_not_locked_when_localization_disabled() {
        let field = make_field("title", FieldType::Text);
        assert!(!is_locale_locked_write(&field, None, false));
    }

    /// `locale_locked_field_names` collects shared leaf names under a non-default
    /// locale, and is empty for the default locale. Drives the draft-snapshot and
    /// join-write locale-locks.
    #[test]
    fn locale_locked_field_names_collects_shared_fields() {
        let title = make_field("title", FieldType::Text); // shared
        let body = make_localized_field("body", FieldType::Text); // localized
        let fields = vec![title, body];

        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: make_locale_config(),
        };
        let locked = locale_locked_field_names(&fields, Some(&de));
        assert!(
            locked.contains("title"),
            "shared field is locked under 'de'"
        );
        assert!(!locked.contains("body"), "localized field is not locked");

        // Default locale → nothing locked.
        let en = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        assert!(locale_locked_field_names(&fields, Some(&en)).is_empty());
    }

    /// Regression: a shared timezone-enabled Date locks BOTH the `start_date`
    /// column and its `start_date_tz` companion under a non-default locale — the
    /// live write skips the whole field, so the draft drop-set must too, or a
    /// non-default-locale edit of the tz survives the snapshot and clobbers the
    /// canonical tz on restore.
    #[test]
    fn locale_locked_field_names_includes_date_tz_companion() {
        let date = FieldDefinition::builder("start_date", FieldType::Date)
            .timezone(true)
            .build(); // shared (not localized)
        let fields = vec![date];

        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: make_locale_config(),
        };
        let locked = locale_locked_field_names(&fields, Some(&de));
        assert!(locked.contains("start_date"), "shared date is locked");
        assert!(
            locked.contains("start_date_tz"),
            "the _tz companion must also be locked, got: {locked:?}"
        );
    }

    /// A shared code field locks its `_lang` companion with it under a
    /// non-default locale, as the live write skips both.
    #[test]
    fn locale_locked_field_names_includes_code_lang_companion() {
        let mut field = localized_code_lang_field("snippet");
        field.localized = false;
        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: make_locale_config(),
        };

        let locked = locale_locked_field_names(&[field], Some(&de));

        assert!(locked.contains("snippet"));
        assert!(
            locked.contains("snippet_lang"),
            "the _lang companion must also be locked, got: {locked:?}"
        );
    }
}
