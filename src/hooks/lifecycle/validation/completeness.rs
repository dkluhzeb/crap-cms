//! Document-level localized completeness check.
//!
//! A `required` **localized** field must be present in every locale of its
//! effective `required_locales` (field override → collection default → default
//! locale only). The write-locale value comes from the submitted data; other
//! locales come from the existing row (column-backed fields) or the field's
//! join table (array / blocks / has-many relationship). Skipped for drafts and
//! when localization is disabled.

use std::collections::HashMap;

use mlua::Lua;

use crate::{
    core::{DocumentFields, FieldDefinition, FieldType, RequiredLocales, validate::FieldError},
    db::{
        DbValue, LocaleContext,
        query::{
            fetch_row_columns,
            helpers::{join_table, locale_column, prefixed_name, walk_leaf_fields},
            localized_join_row_exists,
        },
    },
    hooks::lifecycle::validation::custom::{ValidateCtxSource, run_required_condition_inner},
};

use super::{ValidationCtx, is_empty_value};

/// Stable inputs shared across the recursive `collect_required_localized` walk:
/// the Lua VM (for `required_when` predicates), the submitted document, and the
/// validation + locale context.
struct CompletenessCtx<'a> {
    lua: &'a Lua,
    data: &'a DocumentFields,
    ctx: &'a ValidationCtx<'a>,
    lctx: &'a LocaleContext,
}

/// How a localized field stores its value, which determines the presence check.
#[derive(Clone, Copy, PartialEq)]
enum FieldKind {
    /// Stored in a `field__locale` column on the parent row.
    Scalar,
    /// Stored in a `{collection}_{field}` join table with a `_locale` column
    /// (array / blocks / has-many relationship).
    Join,
}

struct Target {
    data_key: String,
    locales: Vec<String>,
    kind: FieldKind,
}

/// Run the localized completeness check, pushing a `validation.required_locale`
/// error for each (field, locale) that is required but empty.
pub(in crate::hooks::lifecycle::validation) fn check_localized_completeness(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    ctx: &ValidationCtx,
    errors: &mut Vec<FieldError>,
) {
    if ctx.is_draft {
        return;
    }
    let Some(lctx) = ctx.locale_ctx.filter(|c| c.config.is_enabled()) else {
        return;
    };
    let write_locale = lctx.access_locale();

    let cctx = CompletenessCtx {
        lua,
        data,
        ctx,
        lctx,
    };
    let mut targets: Vec<Target> = Vec::new();
    collect_required_localized(&cctx, fields, &mut targets, errors);
    if targets.is_empty() {
        return;
    }

    let existing = read_existing_columns(ctx, &targets);

    for target in &targets {
        for loc in &target.locales {
            // The document's actual post-write state: submitted data overlaid on
            // the existing row. For the write locale a *provided* value wins
            // (even an empty one — that's an explicit clear), but an *omitted*
            // field keeps its existing value (partial-update semantics, matching
            // `check_required`'s `is_update && value.is_none()` exemption). Other
            // locales are never touched by this write, so they read existing.
            let present = if loc == write_locale {
                match data.get(&target.data_key) {
                    Some(value) => submitted_present(target.kind, Some(value)),
                    None => existing_present(ctx, existing.as_ref(), target, loc),
                }
            } else {
                existing_present(ctx, existing.as_ref(), target, loc)
            };

            if !present {
                errors.push(
                    FieldError::with_key(
                        target.data_key.clone(),
                        format!("{} is required in locale '{loc}'", target.data_key),
                        "validation.required_locale",
                    )
                    .with_param("field", target.data_key.clone())
                    .with_param("locale", loc.clone()),
                );
            }
        }
    }
}

/// Whether the submitted (write-locale) value counts as present.
fn submitted_present(kind: FieldKind, value: Option<&serde_json::Value>) -> bool {
    match kind {
        FieldKind::Scalar => !is_empty_value(value),
        // Array / blocks / has-many: a non-empty array (or non-empty JSON
        // string for the parent-column has-many form).
        FieldKind::Join => match value {
            Some(serde_json::Value::Array(a)) => !a.is_empty(),
            Some(serde_json::Value::String(s)) => !s.is_empty(),
            _ => false,
        },
    }
}

/// Whether a non-write locale's value is present on the existing row.
fn existing_present(
    ctx: &ValidationCtx,
    existing_cols: Option<&HashMap<String, DbValue>>,
    target: &Target,
    loc: &str,
) -> bool {
    match target.kind {
        FieldKind::Scalar => locale_column(&target.data_key, loc)
            .ok()
            .and_then(|col| existing_cols.map(|row| db_present(row.get(&col))))
            .unwrap_or(false),
        FieldKind::Join => join_row_exists(ctx, &target.data_key, loc),
    }
}

/// Whether the field's join table has at least one row for this parent + locale.
fn join_row_exists(ctx: &ValidationCtx, data_key: &str, loc: &str) -> bool {
    let Some(id) = ctx.exclude_id else {
        return false; // create: no existing join rows
    };
    let table = join_table(ctx.table, data_key);
    localized_join_row_exists(ctx.conn, &table, id, loc).unwrap_or(false)
}

/// Collect a [`Target`] for every localized required field. The flat-column
/// walk ([`walk_leaf_fields`]) supplies the `group__child` data keys and
/// localization inheritance, matching the migration DDL — only groups carry
/// `localized` down to their children; a `localized` flag on a transparent
/// layout wrapper creates no per-locale columns and therefore must not make
/// the wrapper's children locale-scoped here either. Group sub-fields can't
/// be localized arrays/blocks themselves, so those join tables keep the group
/// prefix in `data_key`.
fn collect_required_localized(
    cctx: &CompletenessCtx<'_>,
    fields: &[FieldDefinition],
    out: &mut Vec<Target>,
    errors: &mut Vec<FieldError>,
) {
    let _ = walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            // Checkboxes always have a value (default off), so `required` is a
            // no-op for them — `check_required` skips them, and completeness
            // must agree, or an unchecked localized checkbox would block writes.
            //
            // A Join is a virtual reverse-relationship with no stored data and
            // no join table of its own; `required`/`localized` are meaningless
            // on it. Skip it here (mirroring the submit-time dispatch's explicit
            // `Join` no-op) — otherwise its presence is checked against a
            // `{collection}_{field}` table that never exists, the query errors to
            // "absent", and a spurious `required_locale` error blocks the write.
            if matches!(field.field_type, FieldType::Checkbox | FieldType::Join) {
                return Ok(());
            }

            let data_key = prefixed_name(prefix, &field.name);

            // A localized field is required either statically (`required`) or
            // when its `required_when` predicate is truthy. The submit-time
            // walker skips the predicate for locale-scoped fields (required
            // is delegated here), so this is the only place it runs for them.
            let required = field.is_locale_scoped(inherited_localized)
                && (field.required || required_when_truthy(cctx, field, &data_key, errors));

            if !required {
                return Ok(());
            }

            let kind = if field.has_parent_column() {
                FieldKind::Scalar
            } else {
                FieldKind::Join
            };
            out.push(Target {
                data_key,
                locales: effective_locales(field, cctx.ctx, cctx.lctx),
                kind,
            });

            Ok(())
        },
    );
}

/// Evaluate a localized field's `required_when` predicate against the submitted
/// document. `false` (no eval) when the field has no predicate. A predicate
/// error pushes a field error and returns `false` (fail-loud, not required).
fn required_when_truthy(
    cctx: &CompletenessCtx<'_>,
    field: &FieldDefinition,
    data_key: &str,
    errors: &mut Vec<FieldError>,
) -> bool {
    let Some(required_when) = field.required_when.as_ref() else {
        return false;
    };

    let func_ref = required_when.reference();

    let operation = if cctx.ctx.exclude_id.is_some() {
        "update"
    } else {
        "create"
    };

    match run_required_condition_inner(
        cctx.lua,
        func_ref,
        &ValidateCtxSource {
            data: cctx.data,
            document: cctx.data,
            collection: cctx.ctx.table,
            field_name: &field.name,
            locale: Some(cctx.lctx.access_locale()),
            operation,
            id: cctx.ctx.exclude_id,
            options: required_when.options(),
        },
    ) {
        Ok(req) => req,
        Err(e) => {
            errors.push(FieldError::new(
                data_key.to_owned(),
                format!("required_when predicate '{func_ref}' failed: {e}"),
            ));
            false
        }
    }
}

/// Resolve a field's effective required locales: its own override, else the
/// collection default, else just the default locale.
fn effective_locales(
    field: &FieldDefinition,
    ctx: &ValidationCtx,
    lctx: &LocaleContext,
) -> Vec<String> {
    match field
        .required_locales
        .as_ref()
        .or(ctx.collection_required_locales)
    {
        Some(RequiredLocales::All) => lctx.config.locales.clone(),
        Some(RequiredLocales::List(l)) => l.clone(),
        None => vec![lctx.config.default_locale.clone()],
    }
}

/// Read the existing row's localized columns for scalar targets, across every
/// required locale (including the write locale, for the omitted-field fallback).
/// Update only — returns `None` on create / when nothing needs reading.
fn read_existing_columns(
    ctx: &ValidationCtx,
    targets: &[Target],
) -> Option<HashMap<String, DbValue>> {
    let id = ctx.exclude_id?;

    let mut cols: Vec<String> = Vec::new();
    for target in targets {
        if target.kind != FieldKind::Scalar {
            continue;
        }
        for loc in &target.locales {
            if let Ok(c) = locale_column(&target.data_key, loc)
                && !cols.contains(&c)
            {
                cols.push(c);
            }
        }
    }
    fetch_row_columns(ctx.conn, ctx.table, &cols, id)
        .ok()
        .flatten()
}

/// Whether a DB column value counts as present for completeness (non-null,
/// non-empty string).
fn db_present(v: Option<&DbValue>) -> bool {
    match v {
        None | Some(DbValue::Null) => false,
        Some(DbValue::Text(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use mlua::Lua;
    use serde_json::json;

    use crate::config::LocaleConfig;
    use crate::core::{DocumentFields, FieldDefinition, FieldType, JoinConfig};
    use crate::db::{LocaleContext, LocaleMode};

    use super::{ValidationCtx, check_localized_completeness};

    fn lua_with_predicate() -> Lua {
        let lua = Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                needs = function(ctx)
                    return ctx.data.kind == "official"
                end,
            }
        "#,
        )
        .exec()
        .unwrap();
        lua
    }

    fn en_de_ctx() -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Default,
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: false,
            },
        }
    }

    /// Regression: a localized field's `required_when` predicate is enforced via
    /// the per-locale completeness gate (the submit-time check skips locale-scoped
    /// fields, delegating to completeness). Previously completeness honored only
    /// the static `required` flag, so a truthy predicate on a localized field was
    /// silently dropped — required nowhere.
    #[test]
    fn localized_required_when_enforced_via_completeness() {
        let lua = lua_with_predicate();
        let fields = vec![
            FieldDefinition::builder("kind", FieldType::Text).build(),
            FieldDefinition::builder("notes", FieldType::Text)
                .localized(true)
                .required_when("validators.needs")
                .build(),
        ];
        let lctx = en_de_ctx();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let ctx = ValidationCtx::builder(&conn, "docs")
            .locale_ctx(Some(&lctx))
            .build();

        // Predicate truthy + `notes` absent → required in the default locale.
        let mut data = DocumentFields::new();
        data.insert("kind".to_string(), json!("official"));
        let mut errors = Vec::new();
        check_localized_completeness(&lua, &fields, &data, &ctx, &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.required_locale")),
            "truthy required_when on a localized field must produce a required_locale error, got: {errors:?}"
        );

        // Predicate falsy → `notes` not required, no error.
        let mut data2 = DocumentFields::new();
        data2.insert("kind".to_string(), json!("casual"));
        let mut errors2 = Vec::new();
        check_localized_completeness(&lua, &fields, &data2, &ctx, &mut errors2);
        assert!(
            errors2.is_empty(),
            "falsy required_when must not require the localized field, got: {errors2:?}"
        );
    }

    /// Regression: a `localized` + `required` Join must NOT block writes. A Join
    /// is a virtual reverse-relationship with no join table of its own, so the
    /// old completeness walk checked presence against `{collection}_{field}`
    /// (which never exists), swallowed the error to "absent", and pushed a
    /// spurious `required_locale` error for every locale — locking the
    /// collection out of every non-draft write.
    #[test]
    fn localized_required_join_does_not_block_write() {
        let lua = Lua::new();
        let fields = vec![
            FieldDefinition::builder("posts", FieldType::Join)
                .localized(true)
                .required(true)
                .join(JoinConfig::new("posts", "author"))
                .build(),
        ];
        let lctx = en_de_ctx();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let ctx = ValidationCtx::builder(&conn, "authors")
            .locale_ctx(Some(&lctx))
            .build();

        let data = DocumentFields::new();
        let mut errors = Vec::new();
        check_localized_completeness(&lua, &fields, &data, &ctx, &mut errors);
        assert!(
            errors.is_empty(),
            "a virtual Join field must never produce a required_locale error, got: {errors:?}"
        );
    }

    /// Regression: a `localized` flag on a transparent layout wrapper must NOT
    /// make the wrapper's children locale-scoped. The migration DDL
    /// (`walk_leaf_fields`) only propagates localization through groups — a
    /// localized Row creates no per-locale columns, so the old bespoke walker
    /// (which propagated the wrapper's flag) could demand locale values for
    /// columns that don't exist and falsely block the write.
    #[test]
    fn localized_layout_wrapper_does_not_scope_children() {
        let lua = Lua::new();
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let lctx = en_de_ctx();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let ctx = ValidationCtx::builder(&conn, "docs")
            .locale_ctx(Some(&lctx))
            .build();

        // `title` is required but NOT locale-scoped (its column is not
        // localized) — completeness must not produce a required_locale error.
        let data = DocumentFields::new();
        let mut errors = Vec::new();
        check_localized_completeness(&lua, &fields, &data, &ctx, &mut errors);
        assert!(
            errors.is_empty(),
            "non-localized child of a 'localized' wrapper must not be \
             locale-gated, got: {errors:?}"
        );
    }
}
