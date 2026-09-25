//! Phase 2 of the per-field parser: fold the parsed parts into a
//! [`FieldDefinition`], with the checks that need the whole field table.

use anyhow::{Result, bail};
use chrono::NaiveDate;
use mlua::{Table, Value};

use crate::{
    core::{FieldDefinition, FieldDefinitionBuilder, FieldType, PickerAppearance},
    hooks::lua_api::parse::{
        fields::constraints::Constraints,
        helpers::{get_bool, get_optional_hook_ref},
    },
};

use super::{ParsedFieldParts, configs::parse_required_locales};

/// A `Join` is a virtual, read-only reverse-relationship with no stored value,
/// so `required` / `localized` / `required_locales` are meaningless on it.
/// Reject them at load instead of silently ignoring them (the previous
/// behavior, which also let a `localized + required` Join wedge non-draft writes
/// — the validation walkers now skip Join as defense-in-depth, but the config is
/// still nonsensical).
fn reject_meaningless_join_flags(
    field_tbl: &Table,
    field_type: &FieldType,
    name: &str,
) -> Result<()> {
    if *field_type != FieldType::Join {
        return Ok(());
    }

    for key in ["required", "localized"] {
        if get_bool(field_tbl, key, false)? {
            bail!(
                "join field '{name}': '{key}' is meaningless — a Join is a virtual, \
                 read-only reverse-relationship with no stored value"
            );
        }
    }
    if parse_required_locales(field_tbl)?.is_some() {
        bail!("join field '{name}': 'required_locales' is meaningless on a virtual Join field");
    }

    Ok(())
}

/// Phase 2 — fold parsed parts into a [`FieldDefinition`] via the builder.
pub(super) fn assemble_field_definition(
    field_tbl: &Table,
    parts: ParsedFieldParts,
) -> Result<FieldDefinition> {
    reject_meaningless_join_flags(field_tbl, &parts.field_type, &parts.name)?;

    let mut fd_builder = FieldDefinition::builder(&parts.name, parts.field_type)
        .required(get_bool(field_tbl, "required", false)?)
        .unique(get_bool(field_tbl, "unique", false)?)
        .index(get_bool(field_tbl, "index", false)?)
        .admin(parts.admin)
        .hooks(parts.hooks)
        .access(parts.access)
        .mcp(parts.mcp)
        .fields(parts.sub_fields)
        .blocks(parts.block_defs)
        .tabs(parts.tab_defs)
        .localized(get_bool(field_tbl, "localized", false)?)
        .has_many(get_bool(field_tbl, "has_many", false)?)
        .hidden(get_bool(field_tbl, "hidden", false)?)
        .options(parts.options);

    if let Some(v) = parse_required_locales(field_tbl)? {
        // Whether `required_locales` is meaningful (the field is locale-scoped —
        // possibly by inheriting localization from an enclosing group) is
        // validated at startup in `startup_checks::validate_required_locales`,
        // where the full field tree (and thus group inheritance) is known. The
        // per-field parser only sees this field's own `localized` flag.
        fd_builder = fd_builder.required_locales(v);
    }

    if let Some(v) = get_optional_hook_ref(field_tbl, "validate", "field validate")? {
        fd_builder = fd_builder.validate(v);
    }
    if let Some(v) = get_optional_hook_ref(field_tbl, "required_when", "required_when")? {
        fd_builder = fd_builder.required_when(v);
    }
    if let Some(v) = parts.default_value {
        fd_builder = fd_builder.default_value(v);
    }
    if let Some(v) = parts.relationship {
        fd_builder = fd_builder.relationship(v);
    }
    if let Some(v) = parts.picker_appearance.clone() {
        fd_builder = fd_builder.picker_appearance(v);
    }

    fd_builder = apply_constraint_bounds(fd_builder, &parts.constraints);
    fd_builder = apply_date_bounds(
        fd_builder,
        field_tbl,
        &parts.name,
        parts.picker_appearance.as_ref(),
    )?;

    if parts.timezone {
        fd_builder = fd_builder.timezone(true);
    }
    if let Some(v) = parts.default_timezone {
        fd_builder = fd_builder.default_timezone(v);
    }
    if let Some(v) = parts.join {
        fd_builder = fd_builder.join(v);
    }

    Ok(fd_builder.build())
}

/// Apply the six numeric-range constraint bounds (`min_rows`/`max_rows`,
/// `min_length`/`max_length`, `min`/`max`) parsed from the field table.
fn apply_constraint_bounds(
    mut builder: FieldDefinitionBuilder,
    constraints: &Constraints,
) -> FieldDefinitionBuilder {
    if let Some(v) = constraints.min_rows {
        builder = builder.min_rows(v);
    }
    if let Some(v) = constraints.max_rows {
        builder = builder.max_rows(v);
    }
    if let Some(v) = constraints.min_length {
        builder = builder.min_length(v);
    }
    if let Some(v) = constraints.max_length {
        builder = builder.max_length(v);
    }
    if let Some(v) = constraints.min {
        builder = builder.min(v);
    }
    if let Some(v) = constraints.max {
        builder = builder.max(v);
    }
    if constraints.integer {
        builder = builder.integer(true);
    }
    builder
}

/// Apply the optional `min_date`/`max_date` bounds. Each bound must be a
/// `YYYY-MM-DD` string: the runtime check compares the submitted value's date
/// part lexically against the bound, so a wrong type or format would make the
/// bound silently never (or always) match. `min_date` after `max_date` is
/// rejected outright, and so is a bound on a `timeOnly` field — a time of day
/// has no date to judge.
fn apply_date_bounds(
    mut builder: FieldDefinitionBuilder,
    field_tbl: &Table,
    name: &str,
    picker: Option<&PickerAppearance>,
) -> Result<FieldDefinitionBuilder> {
    let min = get_date_bound(field_tbl, "min_date", name)?;
    let max = get_date_bound(field_tbl, "max_date", name)?;

    if picker == Some(&PickerAppearance::TimeOnly) && (min.is_some() || max.is_some()) {
        bail!("date field '{name}': min_date/max_date do not apply to a timeOnly picker");
    }

    if let (Some(min), Some(max)) = (&min, &max)
        && min > max
    {
        bail!("date field '{name}': min_date '{min}' is after max_date '{max}'");
    }

    if let Some(v) = min {
        builder = builder.min_date(v);
    }
    if let Some(v) = max {
        builder = builder.max_date(v);
    }

    Ok(builder)
}

/// Read one strict `YYYY-MM-DD` date bound.
fn get_date_bound(field_tbl: &Table, key: &str, name: &str) -> Result<Option<String>> {
    match field_tbl.get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::String(s) => {
            let s = s.to_str()?.to_string();
            if NaiveDate::parse_from_str(&s, "%Y-%m-%d").is_err() {
                bail!("date field '{name}': {key} '{s}' is not a YYYY-MM-DD date");
            }
            Ok(Some(s))
        }
        other => bail!(
            "date field '{name}': {key} must be a YYYY-MM-DD string, got {}",
            other.type_name()
        ),
    }
}
