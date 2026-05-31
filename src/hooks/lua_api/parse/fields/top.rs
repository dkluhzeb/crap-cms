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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::FieldType;

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    #[test]
    fn unique_names_pass() {
        assert!(check_duplicate_field_names(&[text("a"), text("b"), text("c")]).is_ok());
    }

    #[test]
    fn duplicate_top_level_name_errors() {
        let err = check_duplicate_field_names(&[text("a"), text("a")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("Duplicate field name"), "{err}");
    }

    #[test]
    fn duplicate_across_a_transparent_layout_wrapper_errors() {
        // Row is transparent — its child shares the parent scope, so `x`
        // collides with the top-level `x`.
        let row = FieldDefinition::builder("row", FieldType::Row)
            .fields(vec![text("x")])
            .build();
        let err = check_duplicate_field_names(&[text("x"), row])
            .unwrap_err()
            .to_string();
        assert!(err.contains("Duplicate field name"), "{err}");
    }

    #[test]
    fn same_name_in_separate_group_scopes_is_allowed() {
        // Groups are opaque scopes — their inner names aren't compared at
        // this level (each group's scope is checked when it's parsed).
        let ga = FieldDefinition::builder("a", FieldType::Group)
            .fields(vec![text("x")])
            .build();
        let gb = FieldDefinition::builder("b", FieldType::Group)
            .fields(vec![text("x")])
            .build();
        assert!(check_duplicate_field_names(&[ga, gb]).is_ok());
    }
}
