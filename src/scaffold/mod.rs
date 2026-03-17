//! CLI scaffolding commands: init, make collection, make global, make hook, blueprints.
//!
//! Writes plain files to the config directory. No database, no hidden state.

pub mod blueprint;
pub mod collection;
pub mod global;
pub mod hook;
pub mod init;
pub mod job;
pub mod migration;
pub mod templates;

// Re-exports — preserve the flat `scaffold::*` API that callers use.
pub use self::blueprint::{
    blueprint_list, blueprint_remove, blueprint_save, blueprint_use, list_blueprint_names,
};
pub use self::collection::{
    BlockStub, CollectionOptions, FieldStub, TabStub, VALID_FIELD_TYPES, make_collection,
    parse_fields_shorthand,
};
pub use self::global::make_global;
pub use self::hook::{ConditionFieldInfo, HookType, MakeHookOptions, make_hook};
pub use self::init::{InitOptions, init};
pub use self::job::make_job;
pub use self::migration::make_migration;
pub use self::templates::{proto_export, templates_extract, templates_list};

// Re-export the shared title-case helper so submodules can call `super::to_title_case`.
pub(crate) use crate::core::field::to_title_case;

// Re-export from canonical location for backward compatibility.
pub use crate::db::query::validate_slug;

/// Maximum nesting depth for the interactive field wizard.
const MAX_WIZARD_DEPTH: usize = 4;

/// Container field types that prompt for subfields.
const WIZARD_CONTAINER_TYPES: &[&str] = &["group", "array", "row", "collapsible"];

/// Interactive field wizard — prompts for field name, type, required, localized,
/// and recursively prompts for subfields on container types. Returns the field stubs
/// directly (empty vec = no fields).
///
/// `locales_enabled` controls whether the "Localized?" prompt is shown.
#[cfg(not(tarpaulin_include))]
pub fn interactive_field_wizard(locales_enabled: bool) -> anyhow::Result<Vec<FieldStub>> {
    field_loop(locales_enabled, &[])
}

#[cfg(not(tarpaulin_include))]
fn field_loop(locales_enabled: bool, breadcrumb: &[String]) -> anyhow::Result<Vec<FieldStub>> {
    use anyhow::Context as _;
    use dialoguer::{Confirm, Input, Select};

    let depth = breadcrumb.len();
    if depth >= MAX_WIZARD_DEPTH {
        println!(
            "{}Maximum nesting depth ({}) reached — cannot add subfields here.",
            "  ".repeat(depth),
            MAX_WIZARD_DEPTH
        );
        return Ok(vec![]);
    }

    let indent = "  ".repeat(depth);
    if breadcrumb.is_empty() {
        println!("Define fields (empty name to finish):");
    } else {
        println!(
            "{}Define fields for '{}' (empty name to finish):",
            indent,
            breadcrumb.join(" > ")
        );
    }

    let mut fields = Vec::new();

    loop {
        let name: String = Input::new()
            .with_prompt(format!("{}Field name", indent))
            .allow_empty(true)
            .interact_text()
            .context("Failed to read field name")?;

        if name.is_empty() {
            break;
        }

        let type_idx = Select::new()
            .with_prompt(format!("{}Field type", indent))
            .items(VALID_FIELD_TYPES)
            .default(0)
            .interact()
            .context("Failed to read field type")?;
        let field_type = VALID_FIELD_TYPES[type_idx];

        let required = Confirm::new()
            .with_prompt(format!("{}Required?", indent))
            .default(false)
            .interact()
            .context("Failed to read required flag")?;

        let localized = if locales_enabled {
            Confirm::new()
                .with_prompt(format!("{}Localized?", indent))
                .default(false)
                .interact()
                .context("Failed to read localized flag")?
        } else {
            false
        };

        let mut sub_fields = Vec::new();
        let mut sub_blocks = Vec::new();
        let mut sub_tabs = Vec::new();

        if WIZARD_CONTAINER_TYPES.contains(&field_type) {
            let mut child_bc = breadcrumb.to_vec();
            child_bc.push(name.clone());
            sub_fields = field_loop(locales_enabled, &child_bc)?;
        } else if field_type == "blocks" {
            sub_blocks = block_loop(locales_enabled, breadcrumb, &name)?;
        } else if field_type == "tabs" {
            sub_tabs = tab_loop(locales_enabled, breadcrumb, &name)?;
        }

        fields.push(FieldStub {
            name,
            field_type: field_type.to_string(),
            required,
            localized,
            fields: sub_fields,
            blocks: sub_blocks,
            tabs: sub_tabs,
        });
    }

    Ok(fields)
}

#[cfg(not(tarpaulin_include))]
fn block_loop(
    locales_enabled: bool,
    breadcrumb: &[String],
    field_name: &str,
) -> anyhow::Result<Vec<BlockStub>> {
    use anyhow::Context as _;
    use dialoguer::Input;

    let depth = breadcrumb.len();
    let indent = "  ".repeat(depth + 1);
    println!(
        "{}Define blocks for '{}' (empty type to finish):",
        indent,
        if breadcrumb.is_empty() {
            field_name.to_string()
        } else {
            format!("{} > {}", breadcrumb.join(" > "), field_name)
        }
    );

    let mut blocks = Vec::new();

    loop {
        let block_type: String = Input::new()
            .with_prompt(format!("{}Block type", indent))
            .allow_empty(true)
            .interact_text()
            .context("Failed to read block type")?;

        if block_type.is_empty() {
            break;
        }

        let label: String = Input::new()
            .with_prompt(format!("{}Block label", indent))
            .default(to_title_case(&block_type))
            .interact_text()
            .context("Failed to read block label")?;

        let mut child_bc = breadcrumb.to_vec();
        child_bc.push(field_name.to_string());
        child_bc.push(block_type.clone());
        let sub_fields = field_loop(locales_enabled, &child_bc)?;

        blocks.push(BlockStub {
            block_type,
            label,
            fields: sub_fields,
        });
    }

    Ok(blocks)
}

#[cfg(not(tarpaulin_include))]
fn tab_loop(
    locales_enabled: bool,
    breadcrumb: &[String],
    field_name: &str,
) -> anyhow::Result<Vec<TabStub>> {
    use anyhow::Context as _;
    use dialoguer::Input;

    let depth = breadcrumb.len();
    let indent = "  ".repeat(depth + 1);
    println!(
        "{}Define tabs for '{}' (empty label to finish):",
        indent,
        if breadcrumb.is_empty() {
            field_name.to_string()
        } else {
            format!("{} > {}", breadcrumb.join(" > "), field_name)
        }
    );

    let mut tabs = Vec::new();

    loop {
        let label: String = Input::new()
            .with_prompt(format!("{}Tab label", indent))
            .allow_empty(true)
            .interact_text()
            .context("Failed to read tab label")?;

        if label.is_empty() {
            break;
        }

        let mut child_bc = breadcrumb.to_vec();
        child_bc.push(field_name.to_string());
        child_bc.push(label.clone());
        let sub_fields = field_loop(locales_enabled, &child_bc)?;

        tabs.push(TabStub {
            label,
            fields: sub_fields,
        });
    }

    Ok(tabs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_title_case() {
        assert_eq!(to_title_case("posts"), "Posts");
        assert_eq!(to_title_case("site_settings"), "Site Settings");
        assert_eq!(to_title_case("my_cool_thing"), "My Cool Thing");
    }
}
