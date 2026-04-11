//! `make hook` — scaffold a new hook with interactive survey.

use anyhow::{Context as _, Result, anyhow, bail};
use dialoguer::{Input, Select};
use std::path::Path;

use crate::{
    cli::crap_theme,
    commands::MakeAction,
    core::{FieldType, SharedRegistry},
    scaffold::{self, ConditionFieldInfo, HookType, MakeHookOptions},
};

use super::helpers::try_load_registry;

/// Handle the `make hook` subcommand — resolve missing flags via interactive survey.
#[cfg(not(tarpaulin_include))]
pub fn run_hook(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Hook {
        name,
        hook_type,
        collection,
        position,
        field,
        force,
    } = action
    else {
        unreachable!()
    };
    let hook_type = resolve_hook_type(hook_type)?;
    let registry = try_load_registry(config_dir);

    let (collection, is_global) = resolve_hook_collection(collection, &registry)?;
    let position = resolve_hook_position(position, &hook_type)?;
    let field = resolve_hook_field(field, &hook_type, &registry, &collection)?;
    let name = resolve_hook_name(name, &position)?;
    let condition_field = resolve_condition_field(&hook_type, &field, &registry, &collection)?;

    let opts = MakeHookOptions {
        config_dir,
        name: &name,
        hook_type,
        collection: &collection,
        position: &position,
        field: field.as_deref(),
        force,
        condition_field,
        is_global,
    };

    scaffold::make_hook(&opts)
}

/// Resolve hook type from CLI arg or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_hook_type(hook_type: Option<String>) -> Result<HookType> {
    match hook_type {
        Some(t) => HookType::from_name(&t).ok_or_else(|| {
            anyhow!(
                "Unknown hook type '{}' — valid: collection, field, access, condition",
                t
            )
        }),
        None => {
            let items = &["Collection", "Field", "Access", "Condition"];
            let selection = Select::with_theme(&crap_theme())
                .with_prompt("Hook type")
                .items(items)
                .default(0)
                .interact()
                .context("Failed to read hook type selection")?;

            Ok(match selection {
                0 => HookType::Collection,
                1 => HookType::Field,
                2 => HookType::Access,
                _ => HookType::Condition,
            })
        }
    }
}

/// Resolve collection/global slug from CLI arg or interactive selection.
///
/// Returns `(slug, is_global)`.
#[cfg(not(tarpaulin_include))]
fn resolve_hook_collection(
    collection: Option<String>,
    registry: &Option<SharedRegistry>,
) -> Result<(String, bool)> {
    if let Some(c) = collection {
        let is_global = registry
            .as_ref()
            .and_then(|r| r.read().ok())
            .map(|reg| reg.globals.contains_key(c.as_str()))
            .unwrap_or(false);

        return Ok((c, is_global));
    }

    let (collection_slugs, global_slugs) = registry
        .as_ref()
        .and_then(|r| r.read().ok())
        .map(|reg| {
            let mut cs: Vec<String> = reg.collections.keys().map(|s| s.to_string()).collect();
            cs.sort();
            let mut gs: Vec<String> = reg.globals.keys().map(|s| s.to_string()).collect();
            gs.sort();
            (cs, gs)
        })
        .unwrap_or_default();

    if collection_slugs.is_empty() && global_slugs.is_empty() {
        let slug = Input::with_theme(&crap_theme())
            .with_prompt("Collection slug")
            .interact_text()
            .context("Failed to read collection slug")?;

        return Ok((slug, false));
    }

    let mut items: Vec<String> = collection_slugs.clone();
    let global_offset = items.len();

    for g in &global_slugs {
        items.push(format!("{} (global)", g));
    }

    let selection = Select::with_theme(&crap_theme())
        .with_prompt("Collection / Global")
        .items(&items)
        .default(0)
        .interact()
        .context("Failed to read collection selection")?;

    if selection >= global_offset {
        Ok((global_slugs[selection - global_offset].clone(), true))
    } else {
        Ok((collection_slugs[selection].clone(), false))
    }
}

/// Resolve lifecycle position from CLI arg or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_hook_position(position: Option<String>, hook_type: &HookType) -> Result<String> {
    match position {
        Some(p) => {
            if !hook_type.valid_positions().contains(&p.as_str()) {
                bail!(
                    "Invalid position '{}' for {} hook — valid: {}",
                    p,
                    hook_type.label(),
                    hook_type.valid_positions().join(", ")
                );
            }
            Ok(p)
        }
        None => {
            let positions = hook_type.valid_positions();

            if positions.len() == 1 {
                return Ok(positions[0].to_string());
            }

            let prompt = if *hook_type == HookType::Condition {
                "Return type"
            } else {
                "Lifecycle position"
            };

            let selection = Select::with_theme(&crap_theme())
                .with_prompt(prompt)
                .items(positions)
                .default(0)
                .interact()
                .context("Failed to read position selection")?;

            Ok(positions[selection].to_string())
        }
    }
}

/// Resolve field name for field hooks from CLI arg or interactive selection.
#[cfg(not(tarpaulin_include))]
fn resolve_hook_field(
    field: Option<String>,
    hook_type: &HookType,
    registry: &Option<SharedRegistry>,
    collection: &str,
) -> Result<Option<String>> {
    if *hook_type != HookType::Field {
        return Ok(field);
    }

    if let Some(f) = field {
        return Ok(Some(f));
    }

    let field_names: Option<Vec<String>> =
        registry
            .as_ref()
            .and_then(|r| r.read().ok())
            .and_then(|reg| {
                reg.get_collection(collection)
                    .map(|def| def.fields.iter().map(|f| f.name.clone()).collect())
            });

    if let Some(names) = field_names.filter(|n| !n.is_empty()) {
        let selection = Select::with_theme(&crap_theme())
            .with_prompt("Field")
            .items(&names)
            .default(0)
            .interact()
            .context("Failed to read field selection")?;

        Ok(Some(names[selection].clone()))
    } else {
        Ok(Some(
            Input::with_theme(&crap_theme())
                .with_prompt("Field name")
                .interact_text()
                .context("Failed to read field name")?,
        ))
    }
}

/// Resolve hook name from CLI arg or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_hook_name(name: Option<String>, position: &str) -> Result<String> {
    match name {
        Some(n) => Ok(n),
        None => Input::with_theme(&crap_theme())
            .with_prompt("Hook name")
            .default(position.to_string())
            .interact_text()
            .context("Failed to read hook name"),
    }
}

/// Resolve condition field info for condition hooks.
#[cfg(not(tarpaulin_include))]
fn resolve_condition_field(
    hook_type: &HookType,
    field: &Option<String>,
    registry: &Option<SharedRegistry>,
    collection: &str,
) -> Result<Option<ConditionFieldInfo>> {
    if *hook_type != HookType::Condition {
        return Ok(None);
    }

    let field_infos = load_field_infos_from_registry(registry, collection);

    if let Some(f) = field {
        let info = field_infos
            .as_ref()
            .and_then(|infos| infos.iter().find(|i| i.name == *f).cloned())
            .unwrap_or_else(|| ConditionFieldInfo {
                name: f.clone(),
                field_type: "text".to_string(),
                select_options: vec![],
            });

        return Ok(Some(info));
    }

    if let Some(infos) = field_infos.filter(|i| !i.is_empty()) {
        let labels: Vec<String> = infos
            .iter()
            .map(|f| format!("{} ({})", f.name, f.field_type))
            .collect();

        let selection = Select::with_theme(&crap_theme())
            .with_prompt("Watch which field?")
            .items(&labels)
            .default(0)
            .interact()
            .context("Failed to read field selection")?;

        Ok(Some(infos[selection].clone()))
    } else {
        Ok(None)
    }
}

/// Load condition-eligible field infos from the registry.
pub(super) fn load_field_infos_from_registry(
    registry: &Option<SharedRegistry>,
    collection: &str,
) -> Option<Vec<ConditionFieldInfo>> {
    let reg = registry.as_ref()?.read().ok()?;
    let def = reg.get_collection(collection)?;

    Some(
        def.fields
            .iter()
            .filter(|f| {
                !matches!(
                    f.field_type,
                    FieldType::Array
                        | FieldType::Blocks
                        | FieldType::Group
                        | FieldType::Row
                        | FieldType::Collapsible
                        | FieldType::Tabs
                )
            })
            .map(|f| ConditionFieldInfo {
                name: f.name.clone(),
                field_type: format!("{:?}", f.field_type).to_lowercase(),
                select_options: f.options.iter().map(|o| o.value.clone()).collect(),
            })
            .collect(),
    )
}
