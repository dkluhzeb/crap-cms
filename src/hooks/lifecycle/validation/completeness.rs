//! Document-level localized completeness check.
//!
//! A `required` **localized** field must be present in every locale of its
//! effective `required_locales` (field override → collection default → default
//! locale only). The write-locale value comes from the submitted data; other
//! locales come from the existing row (column-backed fields) or the field's
//! join table (array / blocks / has-many relationship). Skipped for drafts and
//! when localization is disabled.

use std::collections::HashMap;

use crate::{
    core::{DocumentFields, FieldDefinition, FieldType, RequiredLocales, validate::FieldError},
    db::{
        DbValue, LocaleContext,
        query::helpers::{join_table, locale_column, prefixed_name},
    },
};

use super::{ValidationCtx, is_empty_value};

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

    let mut targets: Vec<Target> = Vec::new();
    collect_required_localized(fields, "", false, ctx, lctx, &mut targets);
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
    let sql = format!(
        "SELECT 1 FROM \"{table}\" WHERE parent_id = {} AND _locale = {} LIMIT 1",
        ctx.conn.placeholder(1),
        ctx.conn.placeholder(2)
    );
    ctx.conn
        .query_one(
            &sql,
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(loc.to_string()),
            ],
        )
        .ok()
        .flatten()
        .is_some()
}

/// Collect a [`Target`] for every localized required field, recursing through
/// layout containers (mirrors the walker's prefix + inherited-localized
/// traversal). Group sub-fields can't be localized arrays/blocks themselves, so
/// those join tables keep the group prefix in `data_key`.
fn collect_required_localized(
    fields: &[FieldDefinition],
    prefix: &str,
    inherited_localized: bool,
    ctx: &ValidationCtx,
    lctx: &LocaleContext,
    out: &mut Vec<Target>,
) {
    for field in fields {
        let data_key = prefixed_name(prefix, &field.name);
        let localized = inherited_localized || field.localized;

        match field.field_type {
            FieldType::Group => {
                collect_required_localized(&field.fields, &data_key, localized, ctx, lctx, out);
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_required_localized(&field.fields, prefix, localized, ctx, lctx, out);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_required_localized(&tab.fields, prefix, localized, ctx, lctx, out);
                }
            }
            // Checkboxes always have a value (default off), so `required` is a
            // no-op for them — `check_required` skips them, and completeness
            // must agree, or an unchecked localized checkbox would block writes.
            FieldType::Checkbox => {}
            _ => {
                if field.required && field.is_locale_scoped(inherited_localized) {
                    let kind = if field.has_parent_column() {
                        FieldKind::Scalar
                    } else {
                        FieldKind::Join
                    };
                    out.push(Target {
                        data_key,
                        locales: effective_locales(field, ctx, lctx),
                        kind,
                    });
                }
            }
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
    if cols.is_empty() {
        return None;
    }

    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {col_list} FROM \"{}\" WHERE id = {}",
        ctx.table,
        ctx.conn.placeholder(1)
    );
    let row = ctx
        .conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])
        .ok()
        .flatten()?;

    let mut map = HashMap::new();
    for c in &cols {
        if let Some(v) = row.get_named(c) {
            map.insert(c.clone(), v.clone());
        }
    }
    Some(map)
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
