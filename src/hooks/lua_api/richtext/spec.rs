//! The `crap.richtext.register_node(name, spec)` spec table, and the checks a
//! node registration must pass — one set shared by the init VM (which writes
//! the shared registry) and every pool VM (which only stores render
//! functions), so both refuse exactly the same registrations.

use mlua::{Error::RuntimeError, FromLua, Function, Lua, Result as LuaResult, Table, Value};

use crate::{
    core::{FieldDefinition, richtext::validate_node_name},
    hooks::lua_api::{
        parse::{deny_unknown_keys, fields::parse_fields, get_bool, get_string_strict},
        utils::lua_err,
    },
    typegen::lua::LuaAnnotation,
};

/// Spec for registering a custom richtext node. Parsed from the Lua
/// table the user passes to `crap.richtext.register_node(name, spec)`.
///
/// `attrs` is the only Lua-typed field that's converted to a Rust
/// representation eagerly (via `parse_fields`) so the call site can
/// run the type-allow-list validation against typed
/// `FieldDefinition`s rather than re-walking a Lua table. `render`
/// stays as `mlua::Function` because it's invoked from Rust later, and
/// `mlua::Function` is the natural type for that.
#[derive(LuaAnnotation)]
#[lua(class = "crap.RichtextNodeSpec")]
pub(crate) struct RichtextNodeSpec {
    /// Display label (defaults to `name`).
    #[lua(optional)]
    pub(crate) label: Option<String>,
    /// Whether the node is inline (default: `false` = block).
    #[lua(optional)]
    pub(crate) inline: bool,
    /// Attribute definitions (scalar types only: text, number, textarea,
    /// select, radio, checkbox, date, email, json, code). Use
    /// `crap.fields.*` factory functions. Settings with no effect on a node
    /// attr (`unique`, `index`, `localized`, `has_many`, `required_when`,
    /// `access`, `before_change` / `after_change` / `after_read` hooks,
    /// `admin.condition`, `admin.position`, `mcp.description`) are refused.
    #[lua(ty = "crap.FieldDefinition[]", optional)]
    pub(crate) attrs: Vec<FieldDefinition>,
    /// Attr names to include in FTS search index.
    #[lua(optional)]
    pub(crate) searchable_attrs: Vec<String>,
    /// Server-side render function. Receives the node attrs as a Lua
    /// table; returns the rendered HTML string.
    #[lua(ty = "fun(attrs: table): string", optional)]
    pub(crate) render: Option<Function>,
}

impl FromLua for RichtextNodeSpec {
    fn from_lua(value: Value, lua: &Lua) -> LuaResult<Self> {
        let Value::Table(tbl) = value else {
            return Err(RuntimeError(format!(
                "crap.richtext.register_node spec must be a table, got {}",
                value.type_name()
            )));
        };

        deny_unknown_keys(
            &tbl,
            "crap.richtext.register_node spec",
            &["label", "inline", "attrs", "searchable_attrs", "render"],
        )
        .map_err(lua_err)?;

        let attrs = match tbl.get::<Option<Table>>("attrs")? {
            Some(attrs_tbl) => parse_fields(lua, &attrs_tbl)
                .map_err(|e| RuntimeError(format!("Invalid node attrs: {e:#}")))?,
            None => Vec::new(),
        };

        // Strict reads: `tbl.get::<Option<bool>>` applies Lua truthiness to
        // ANY value (so `inline = "false"` would register as inline TRUE),
        // and `Option<String>` coerces silently. Use the strict helpers so a
        // wrong-typed value errors at load, matching the project invariant.
        Ok(Self {
            label: get_string_strict(&tbl, "label", "crap.richtext.register_node spec")?,
            inline: get_bool(&tbl, "inline", false)?,
            attrs,
            searchable_attrs: parse_searchable_attrs(&tbl)?,
            render: tbl.get::<Option<Function>>("render")?,
        })
    }
}

/// The `searchable_attrs` list: absent → empty; a non-string entry is an error.
fn parse_searchable_attrs(tbl: &Table) -> LuaResult<Vec<String>> {
    let Some(sa_tbl) = tbl.get::<Option<Table>>("searchable_attrs")? else {
        return Ok(Vec::new());
    };

    sa_tbl
        .sequence_values::<String>()
        .collect::<LuaResult<Vec<_>>>()
        .map_err(|e| {
            RuntimeError(format!(
                "crap.richtext.register_node: `searchable_attrs` must be \
                 an array of strings: {e}"
            ))
        })
}

/// Every check a node registration must pass: a valid name, attrs of scalar
/// types without inert settings, and `searchable_attrs` naming real attrs.
pub(super) fn validate_spec(name: &str, spec: &RichtextNodeSpec) -> LuaResult<()> {
    validate_node_name(name).map_err(RuntimeError)?;
    validate_node_attrs(name, &spec.attrs)?;
    validate_searchable_attrs(name, &spec.attrs, &spec.searchable_attrs)
}

/// Every attr must use a scalar type and carry no setting that has no effect
/// on a node attr.
fn validate_node_attrs(name: &str, attrs: &[FieldDefinition]) -> LuaResult<()> {
    for f in attrs {
        if !f.field_type.is_node_attr_type() {
            return Err(RuntimeError(format!(
                "Node attr '{}' has type '{}' which is not allowed as a node attribute. \
                 Allowed types: text, number, textarea, select, radio, checkbox, date, email, json, code",
                f.name,
                f.field_type.as_str(),
            )));
        }

        let inert = inert_attr_settings(f);

        if !inert.is_empty() {
            return Err(RuntimeError(format!(
                "Node '{name}' attr '{}': {} {} no effect on a node attribute — remove {}. \
                 A node attr lives inside the rich text value: it has no column, no \
                 per-locale value, no access rules and no write/read lifecycle of its own; \
                 it supports `required`, `validate`, `hooks.before_validate` and the \
                 type's value constraints.",
                f.name,
                inert.join(", "),
                if inert.len() == 1 { "has" } else { "have" },
                if inert.len() == 1 { "it" } else { "them" },
            )));
        }
    }

    Ok(())
}

/// The settings on `f` that do nothing on a node attr: it has no column
/// (`unique`, `index`), no per-locale value (`localized`, `required_locales`),
/// a single-value editor (`has_many`), no conditional presence
/// (`required_when`), no access rules and no change/read lifecycle, no admin
/// display condition or form placement and no MCP schema entry.
fn inert_attr_settings(f: &FieldDefinition) -> Vec<&'static str> {
    [
        (f.unique, "unique"),
        (f.index, "index"),
        (f.localized, "localized"),
        (f.required_locales.is_some(), "required_locales"),
        (f.has_many, "has_many"),
        (f.required_when.is_some(), "required_when"),
        (f.access.read.is_some(), "access.read"),
        (f.access.create.is_some(), "access.create"),
        (f.access.update.is_some(), "access.update"),
        (!f.hooks.before_change.is_empty(), "hooks.before_change"),
        (!f.hooks.after_change.is_empty(), "hooks.after_change"),
        (!f.hooks.after_read.is_empty(), "hooks.after_read"),
        (f.admin.condition.is_some(), "admin.condition"),
        (f.admin.position.is_some(), "admin.position"),
        (f.mcp.description.is_some(), "mcp.description"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect()
}

/// Validate every entry in `searchable_attrs` references a real attr.
fn validate_searchable_attrs(
    name: &str,
    attrs: &[FieldDefinition],
    searchable_attrs: &[String],
) -> LuaResult<()> {
    let attr_names: Vec<&str> = attrs.iter().map(|a| a.name.as_str()).collect();

    let Some(unknown) = searchable_attrs
        .iter()
        .find(|sa| !attr_names.contains(&sa.as_str()))
    else {
        return Ok(());
    };

    Err(RuntimeError(format!(
        "Node '{}': searchable_attrs references unknown attr '{}'.\n\
         Available attrs: [{}]",
        name,
        unknown,
        attr_names.join(", "),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldHooks, FieldType, HookRef};

    fn spec(attrs: Vec<FieldDefinition>, searchable: &[&str]) -> RichtextNodeSpec {
        RichtextNodeSpec {
            label: None,
            inline: false,
            attrs,
            searchable_attrs: searchable.iter().map(ToString::to_string).collect(),
            render: None,
        }
    }

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    #[test]
    fn a_plain_spec_is_valid() {
        assert!(validate_spec("cta", &spec(vec![text("url")], &["url"])).is_ok());
    }

    /// Regression: settings with no effect on a node attr only logged a
    /// warning, so an author relying on `access.read` or `unique` got neither.
    #[test]
    fn inert_attr_settings_are_refused() {
        let mut attr = text("title");
        attr.unique = true;
        attr.has_many = true;
        attr.hooks = FieldHooks {
            after_read: vec![HookRef::new("hooks.x")],
            ..Default::default()
        };
        attr.admin.position = Some("sidebar".to_string());

        let err = validate_spec("cta", &spec(vec![attr], &[]))
            .unwrap_err()
            .to_string();

        assert!(err.contains("Node 'cta' attr 'title'"), "{err}");
        assert!(
            err.contains("unique, has_many, hooks.after_read, admin.position"),
            "{err}"
        );
    }

    #[test]
    fn supported_attr_settings_are_accepted() {
        let mut attr = text("title");
        attr.required = true;
        attr.min_length = Some(2);
        attr.hooks = FieldHooks {
            before_validate: vec![HookRef::new("hooks.trim")],
            ..Default::default()
        };

        assert!(validate_spec("cta", &spec(vec![attr], &[])).is_ok());
    }

    #[test]
    fn unknown_searchable_attr_is_refused() {
        let err = validate_spec("cta", &spec(vec![text("title")], &["title", "nope"]))
            .unwrap_err()
            .to_string();

        assert!(err.contains("nope"), "{err}");
    }
}
