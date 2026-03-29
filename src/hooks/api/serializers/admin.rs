//! Lua table serializer for field admin configuration.

use mlua::{Lua, Result as LuaResult, Table};

use crate::core::FieldAdmin;

use super::helpers::localized_string_to_lua;

/// Convert a `FieldAdmin` to a Lua table. Returns `None` if no properties are set.
pub(super) fn field_admin_to_lua(lua: &Lua, admin: &FieldAdmin) -> LuaResult<Option<Table>> {
    let tbl = lua.create_table()?;
    let mut has_any = false;

    if let Some(ref l) = admin.label {
        tbl.set("label", localized_string_to_lua(lua, l)?)?;
        has_any = true;
    }
    if let Some(ref p) = admin.placeholder {
        tbl.set("placeholder", localized_string_to_lua(lua, p)?)?;
        has_any = true;
    }
    if let Some(ref d) = admin.description {
        tbl.set("description", localized_string_to_lua(lua, d)?)?;
        has_any = true;
    }
    if admin.hidden {
        tbl.set("hidden", true)?;
        has_any = true;
    }
    if admin.readonly {
        tbl.set("readonly", true)?;
        has_any = true;
    }
    if let Some(ref w) = admin.width {
        tbl.set("width", w.as_str())?;
        has_any = true;
    }
    if !admin.collapsed {
        tbl.set("collapsed", false)?;
        has_any = true;
    }
    if let Some(ref lf) = admin.label_field {
        tbl.set("label_field", lf.as_str())?;
        has_any = true;
    }
    if let Some(ref rl) = admin.row_label {
        tbl.set("row_label", rl.as_str())?;
        has_any = true;
    }
    if let Some(ref ls) = admin.labels_singular {
        let labels = lua.create_table()?;
        labels.set("singular", localized_string_to_lua(lua, ls)?)?;

        if let Some(ref lp) = admin.labels_plural {
            labels.set("plural", localized_string_to_lua(lua, lp)?)?;
        }
        tbl.set("labels", labels)?;
        has_any = true;
    } else if let Some(ref lp) = admin.labels_plural {
        let labels = lua.create_table()?;
        labels.set("plural", localized_string_to_lua(lua, lp)?)?;
        tbl.set("labels", labels)?;
        has_any = true;
    }
    if let Some(ref pos) = admin.position {
        tbl.set("position", pos.as_str())?;
        has_any = true;
    }
    if let Some(ref cond) = admin.condition {
        tbl.set("condition", cond.as_str())?;
        has_any = true;
    }
    if let Some(ref s) = admin.step {
        tbl.set("step", s.as_str())?;
        has_any = true;
    }
    if let Some(r) = admin.rows {
        tbl.set("rows", r)?;
        has_any = true;
    }
    if let Some(ref lang) = admin.language {
        tbl.set("language", lang.as_str())?;
        has_any = true;
    }
    if !admin.features.is_empty() {
        let features = lua.create_table()?;
        for (i, feat) in admin.features.iter().enumerate() {
            features.set(i + 1, feat.as_str())?;
        }
        tbl.set("features", features)?;
        has_any = true;
    }
    if let Some(ref p) = admin.picker {
        tbl.set("picker", p.as_str())?;
        has_any = true;
    }
    if let Some(ref fmt) = admin.richtext_format {
        tbl.set("format", fmt.as_str())?;
        has_any = true;
    }
    if !admin.nodes.is_empty() {
        let nodes_tbl = lua.create_table()?;
        for (i, n) in admin.nodes.iter().enumerate() {
            nodes_tbl.set(i + 1, n.as_str())?;
        }
        tbl.set("nodes", nodes_tbl)?;
        has_any = true;
    }
    Ok(if has_any { Some(tbl) } else { None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldAdmin, LocalizedString};

    #[test]
    fn test_field_admin_to_lua_empty_returns_none() {
        let lua = mlua::Lua::new();
        let admin = FieldAdmin::default();
        let result = field_admin_to_lua(&lua, &admin).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_field_admin_to_lua_with_properties() {
        let lua = mlua::Lua::new();
        let admin = FieldAdmin::builder()
            .label(LocalizedString::Plain("Title".to_string()))
            .hidden(true)
            .build();
        let result = field_admin_to_lua(&lua, &admin).unwrap();
        assert!(result.is_some());
        let tbl = result.unwrap();
        assert_eq!(tbl.get::<String>("label").unwrap(), "Title");
        assert!(tbl.get::<bool>("hidden").unwrap());
    }
}
