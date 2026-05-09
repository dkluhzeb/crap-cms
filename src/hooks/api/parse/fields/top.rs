//! Top-level entry point that drives `parse_single_field` over an array of
//! field tables and rejects duplicate field names.

use std::collections::HashSet;

use anyhow::{Result, bail};
use mlua::{Lua, Table};

use crate::core::{FieldDefinition, field::flatten_array_sub_fields};

use super::single::parse_single_field;

pub(crate) fn parse_fields(lua: &Lua, fields_tbl: &Table) -> Result<Vec<FieldDefinition>> {
    let fields: Vec<FieldDefinition> = fields_tbl
        .clone()
        .sequence_values::<Table>()
        .map(|pair| parse_single_field(lua, &pair?))
        .collect::<Result<Vec<_>>>()?;

    check_duplicate_field_names(&fields)?;

    Ok(fields)
}

fn check_duplicate_field_names(fields: &[FieldDefinition]) -> Result<()> {
    let mut seen: HashSet<&str> = HashSet::new();

    for f in flatten_array_sub_fields(fields) {
        if !seen.insert(f.name.as_str()) {
            bail!(
                "Duplicate field name '{}' in the same scope — field names must be unique per level (layout wrappers are transparent)",
                f.name
            );
        }
    }

    Ok(())
}
