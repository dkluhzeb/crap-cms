//! Lua table serializer for field admin configuration.

use mlua::{Lua, Result as LuaResult, Table};

use crate::core::FieldAdmin;

use super::helpers::localized_string_to_lua;

/// Convert a `FieldAdmin` to a Lua table. Returns `None` if all defaults.
pub(super) fn field_admin_to_lua(lua: &Lua, admin: &FieldAdmin) -> LuaResult<Option<Table>> {
    if *admin == FieldAdmin::default() {
        return Ok(None);
    }

    let tbl = lua.create_table()?;

    if let Some(ref v) = admin.label {
        tbl.set("label", localized_string_to_lua(lua, v)?)?;
    }
    if let Some(ref v) = admin.placeholder {
        tbl.set("placeholder", localized_string_to_lua(lua, v)?)?;
    }
    if let Some(ref v) = admin.description {
        tbl.set("description", localized_string_to_lua(lua, v)?)?;
    }
    if admin.hidden {
        tbl.set("hidden", true)?;
    }
    if admin.readonly {
        tbl.set("readonly", true)?;
    }
    if let Some(ref v) = admin.width {
        tbl.set("width", v.as_str())?;
    }
    if !admin.collapsed {
        tbl.set("collapsed", false)?;
    }
    if let Some(ref v) = admin.label_field {
        tbl.set("label_field", v.as_str())?;
    }
    if let Some(ref v) = admin.row_label {
        tbl.set("row_label", v.as_str())?;
    }
    if let Some(ref v) = admin.position {
        tbl.set("position", v.as_str())?;
    }
    if let Some(ref v) = admin.condition {
        tbl.set("condition", v.as_str())?;
    }
    if let Some(ref v) = admin.step {
        tbl.set("step", v.as_str())?;
    }
    if let Some(v) = admin.rows {
        tbl.set("rows", v)?;
    }
    if let Some(ref v) = admin.language {
        tbl.set("language", v.as_str())?;
    }
    if let Some(ref v) = admin.picker {
        tbl.set("picker", v.as_str())?;
    }
    if let Some(ref v) = admin.richtext_format {
        tbl.set("format", v.as_str())?;
    }

    // Labels subtable
    if admin.labels_singular.is_some() || admin.labels_plural.is_some() {
        let labels = lua.create_table()?;
        if let Some(ref v) = admin.labels_singular {
            labels.set("singular", localized_string_to_lua(lua, v)?)?;
        }
        if let Some(ref v) = admin.labels_plural {
            labels.set("plural", localized_string_to_lua(lua, v)?)?;
        }
        tbl.set("labels", labels)?;
    }

    // Sequence tables
    if !admin.features.is_empty() {
        let seq = lua.create_table()?;
        for (i, v) in admin.features.iter().enumerate() {
            seq.set(i + 1, v.as_str())?;
        }
        tbl.set("features", seq)?;
    }
    if !admin.nodes.is_empty() {
        let seq = lua.create_table()?;
        for (i, v) in admin.nodes.iter().enumerate() {
            seq.set(i + 1, v.as_str())?;
        }
        tbl.set("nodes", seq)?;
    }
    if !admin.languages.is_empty() {
        let seq = lua.create_table()?;
        for (i, v) in admin.languages.iter().enumerate() {
            seq.set(i + 1, v.as_str())?;
        }
        tbl.set("languages", seq)?;
    }

    Ok(Some(tbl))
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

    /// Regression: the serializer must include `languages` so a Lua-side
    /// roundtrip (`crap.collections.config.list()` → mutate → `crap.collections.define()`)
    /// doesn't silently drop the code-field language allow-list. Plugins like
    /// `seo.lua` do exactly this roundtrip; without `languages` in this
    /// serializer the SEO plugin would clobber the user's code-field picker
    /// every startup.
    #[test]
    fn test_field_admin_to_lua_includes_languages() {
        let lua = mlua::Lua::new();
        let admin = FieldAdmin::builder()
            .languages(vec!["javascript".to_string(), "python".to_string()])
            .build();
        let result = field_admin_to_lua(&lua, &admin).unwrap();
        let tbl = result.expect("non-default admin should produce Some(Table)");
        let langs: Table = tbl.get("languages").expect("languages key must be set");
        let collected: Vec<String> = langs
            .sequence_values::<String>()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(collected, vec!["javascript", "python"]);
    }
}
