//! `make` command — scaffold collections, globals, hooks, and jobs.

use anyhow::{Context as _, Result};
use std::path::Path;

/// Dispatch the `make` subcommand.
#[cfg(not(tarpaulin_include))] // interactive dispatcher — uses dialoguer prompts
pub fn run(action: super::MakeAction) -> Result<()> {
    match action {
        super::MakeAction::Collection { config, slug, fields, no_timestamps, auth, upload, versions, no_input, force } => {
            make_collection_command(&config, slug, fields, no_timestamps, auth, upload, versions, !no_input, force)
        }
        super::MakeAction::Global { config, slug, fields, force } => {
            let slug = match slug {
                Some(s) => s,
                None => {
                    use dialoguer::Input;
                    Input::<String>::new()
                        .with_prompt("Global slug")
                        .validate_with(|input: &String| -> Result<(), String> {
                            crate::scaffold::validate_slug(input).map_err(|e| e.to_string())
                        })
                        .interact_text()
                        .context("Failed to read global slug")?
                }
            };
            crate::scaffold::make_global(&config, &slug, fields.as_deref(), force)
        }
        super::MakeAction::Hook { config, name, hook_type, collection, position, field, force } => {
            make_hook_command(&config, name, hook_type, collection, position, field, force)
        }
        super::MakeAction::Job { config, slug, schedule, queue, retries, timeout, force } => {
            let slug = match slug {
                Some(s) => s,
                None => {
                    use dialoguer::Input;
                    Input::<String>::new()
                        .with_prompt("Job slug")
                        .validate_with(|input: &String| -> Result<(), String> {
                            crate::scaffold::validate_slug(input).map_err(|e| e.to_string())
                        })
                        .interact_text()
                        .context("Failed to read job slug")?
                }
            };
            crate::scaffold::make_job(&config, &slug, schedule.as_deref(), queue.as_deref(), retries, timeout, force)
        }
    }
}

/// Handle the `make collection` subcommand — resolve missing args via interactive survey.
#[cfg(not(tarpaulin_include))] // uses dialoguer prompts throughout
pub(crate) fn make_collection_command(
    config_dir: &Path,
    slug: Option<String>,
    fields: Option<String>,
    no_timestamps: bool,
    auth: bool,
    upload: bool,
    versions: bool,
    interactive: bool,
    force: bool,
) -> Result<()> {
    use dialoguer::{Input, Select, Confirm};

    // 1. Resolve slug
    let slug = match slug {
        Some(s) => s,
        None if interactive => {
            Input::<String>::new()
                .with_prompt("Collection slug")
                .validate_with(|input: &String| -> Result<(), String> {
                    if input.is_empty() {
                        return Err("Slug cannot be empty".into());
                    }
                    if !input.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                        return Err("Use lowercase letters, digits, and underscores only".into());
                    }
                    if input.starts_with('_') {
                        return Err("Slug cannot start with underscore".into());
                    }
                    Ok(())
                })
                .interact_text()
                .context("Failed to read collection slug")?
        }
        None => anyhow::bail!("Collection slug is required (or omit --no-input for interactive mode)"),
    };

    // 2. Resolve fields — survey when interactive and not provided via --fields
    let fields_shorthand = match fields {
        Some(s) => Some(s),
        None if interactive => {
            println!("Define fields (empty name to finish):");
            let mut parts: Vec<String> = Vec::new();

            loop {
                let name: String = Input::new()
                    .with_prompt("Field name")
                    .allow_empty(true)
                    .interact_text()
                    .context("Failed to read field name")?;

                if name.is_empty() {
                    break;
                }

                let type_idx = Select::new()
                    .with_prompt("Field type")
                    .items(crate::scaffold::VALID_FIELD_TYPES)
                    .default(0)
                    .interact()
                    .context("Failed to read field type")?;
                let field_type = crate::scaffold::VALID_FIELD_TYPES[type_idx];

                let required = Confirm::new()
                    .with_prompt("Required?")
                    .default(false)
                    .interact()
                    .context("Failed to read required flag")?;

                // Only prompt for localized if localization is enabled in config
                let localized = if has_locales_enabled(config_dir) {
                    Confirm::new()
                        .with_prompt("Localized?")
                        .default(false)
                        .interact()
                        .context("Failed to read localized flag")?
                } else {
                    false
                };

                let mut part = format!("{}:{}", name, field_type);
                if required {
                    part.push_str(":required");
                }
                if localized {
                    part.push_str(":localized");
                }
                parts.push(part);
            }

            if parts.is_empty() {
                None // will use default title:text:required
            } else {
                Some(parts.join(","))
            }
        }
        None => None, // non-interactive, use defaults
    };

    // 3. Resolve timestamps (only prompt in interactive mode)
    let no_timestamps = if no_timestamps {
        true
    } else if interactive {
        let timestamps = Confirm::new()
            .with_prompt("Enable timestamps?")
            .default(true)
            .interact()
            .context("Failed to read timestamps preference")?;
        !timestamps
    } else {
        false
    };

    // 4. Resolve auth (only prompt in interactive mode)
    let auth = if auth {
        true
    } else if interactive {
        Confirm::new()
            .with_prompt("Auth collection (email/password login)?")
            .default(false)
            .interact()
            .context("Failed to read auth preference")?
    } else {
        false
    };

    // 5. Resolve upload (only prompt in interactive mode)
    let upload = if upload {
        true
    } else if interactive {
        Confirm::new()
            .with_prompt("Upload collection (file uploads)?")
            .default(false)
            .interact()
            .context("Failed to read upload preference")?
    } else {
        false
    };

    // 6. Resolve versioning (only prompt in interactive mode)
    let versions = if versions {
        true
    } else if interactive {
        Confirm::new()
            .with_prompt("Enable versioning (draft/publish workflow)?")
            .default(false)
            .interact()
            .context("Failed to read versioning preference")?
    } else {
        false
    };

    crate::scaffold::make_collection(config_dir, &slug, fields_shorthand.as_deref(), no_timestamps, auth, upload, versions, force)
}

/// Handle the `make hook` subcommand — resolve missing flags via interactive survey.
#[cfg(not(tarpaulin_include))] // uses dialoguer prompts throughout
fn make_hook_command(
    config_dir: &Path,
    name: Option<String>,
    hook_type: Option<String>,
    collection: Option<String>,
    position: Option<String>,
    field: Option<String>,
    force: bool,
) -> Result<()> {
    use dialoguer::{Input, Select};

    // 1. Resolve hook type
    let hook_type = match hook_type {
        Some(t) => crate::scaffold::HookType::from_str(&t)
            .ok_or_else(|| anyhow::anyhow!(
                "Unknown hook type '{}' — valid: collection, field, access, condition", t
            ))?,
        None => {
            let items = &["Collection", "Field", "Access", "Condition"];
            let selection = Select::new()
                .with_prompt("Hook type")
                .items(items)
                .default(0)
                .interact()
                .context("Failed to read hook type selection")?;
            match selection {
                0 => crate::scaffold::HookType::Collection,
                1 => crate::scaffold::HookType::Field,
                2 => crate::scaffold::HookType::Access,
                _ => crate::scaffold::HookType::Condition,
            }
        }
    };

    // 2. Resolve collection/global — try loading registry for choices, fall back to text input
    let (collection, is_global) = match collection {
        Some(c) => {
            // Auto-detect: check if it's a global slug
            let is_global = try_load_global_slugs(config_dir)
                .map(|slugs| slugs.contains(&c))
                .unwrap_or(false);
            (c, is_global)
        }
        None => {
            let collection_slugs = try_load_collection_slugs(config_dir).unwrap_or_default();
            let global_slugs = try_load_global_slugs(config_dir).unwrap_or_default();

            if !collection_slugs.is_empty() || !global_slugs.is_empty() {
                // Build merged list: collections first, then globals tagged
                let mut items: Vec<String> = collection_slugs.clone();
                let global_offset = items.len();
                for g in &global_slugs {
                    items.push(format!("{} (global)", g));
                }

                let selection = Select::new()
                    .with_prompt("Collection / Global")
                    .items(&items)
                    .default(0)
                    .interact()
                    .context("Failed to read collection selection")?;

                if selection >= global_offset {
                    (global_slugs[selection - global_offset].clone(), true)
                } else {
                    (collection_slugs[selection].clone(), false)
                }
            } else {
                let slug = Input::<String>::new()
                    .with_prompt("Collection slug")
                    .interact_text()
                    .context("Failed to read collection slug")?;
                (slug, false)
            }
        }
    };

    // 3. Resolve position
    let position = match position {
        Some(p) => {
            if !hook_type.valid_positions().contains(&p.as_str()) {
                anyhow::bail!(
                    "Invalid position '{}' for {} hook — valid: {}",
                    p, hook_type.label(), hook_type.valid_positions().join(", ")
                );
            }
            p
        }
        None => {
            let positions = hook_type.valid_positions();
            if positions.len() == 1 {
                // Single valid position (e.g., condition hooks) — skip prompt
                positions[0].to_string()
            } else {
                let prompt = if hook_type == crate::scaffold::HookType::Condition {
                    "Return type"
                } else {
                    "Lifecycle position"
                };
                let selection = Select::new()
                    .with_prompt(prompt)
                    .items(positions)
                    .default(0)
                    .interact()
                    .context("Failed to read position selection")?;
                positions[selection].to_string()
            }
        }
    };

    // 4. Resolve field name (field hooks only)
    let field = if hook_type == crate::scaffold::HookType::Field {
        match field {
            Some(f) => Some(f),
            None => {
                let field_names = try_load_field_names(config_dir, &collection);
                if let Some(names) = field_names.filter(|n| !n.is_empty()) {
                    let selection = Select::new()
                        .with_prompt("Field")
                        .items(&names)
                        .default(0)
                        .interact()
                        .context("Failed to read field selection")?;
                    Some(names[selection].clone())
                } else {
                    Some(Input::<String>::new()
                        .with_prompt("Field name")
                        .interact_text()
                        .context("Failed to read field name")?)
                }
            }
        }
    } else {
        field // pass through even if set (make_hook ignores it for non-field hooks)
    };

    // 5. Resolve name
    let name = match name {
        Some(n) => n,
        None => {
            let default = position.clone();
            Input::<String>::new()
                .with_prompt("Hook name")
                .default(default)
                .interact_text()
                .context("Failed to read hook name")?
        }
    };

    // 6. For condition hooks: resolve watched field with type info
    let condition_field = if hook_type == crate::scaffold::HookType::Condition {
        let field_infos = try_load_field_infos(config_dir, &collection);
        if let Some(ref f) = field {
            // CLI --field flag provided — look up type info from registry if available
            if let Some(ref infos) = field_infos {
                if let Some(info) = infos.iter().find(|i| i.name == *f) {
                    Some(info.clone())
                } else {
                    Some(crate::scaffold::ConditionFieldInfo {
                        name: f.clone(),
                        field_type: "text".to_string(),
                        select_options: vec![],
                    })
                }
            } else {
                Some(crate::scaffold::ConditionFieldInfo {
                    name: f.clone(),
                    field_type: "text".to_string(),
                    select_options: vec![],
                })
            }
        } else if let Some(infos) = field_infos.filter(|i| !i.is_empty()) {
            let labels: Vec<String> = infos.iter()
                .map(|f| format!("{} ({})", f.name, f.field_type))
                .collect();
            let selection = Select::new()
                .with_prompt("Watch which field?")
                .items(&labels)
                .default(0)
                .interact()
                .context("Failed to read field selection")?;
            Some(infos[selection].clone())
        } else {
            None
        }
    } else {
        None
    };

    let opts = crate::scaffold::MakeHookOptions {
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

    crate::scaffold::make_hook(&opts)
}

/// Check if localization is enabled in the config dir's crap.toml.
pub fn has_locales_enabled(config_dir: &Path) -> bool {
    let toml_path = config_dir.join("crap.toml");
    let content = std::fs::read_to_string(&toml_path).unwrap_or_default();
    let table: toml::Table = content.parse().unwrap_or_default();
    table.get("locale")
        .and_then(|v| v.get("locales"))
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
}

/// Try to load collection slugs from the config dir for interactive selection.
pub fn try_load_collection_slugs(config_dir: &Path) -> Option<Vec<String>> {
    let config_dir = config_dir.canonicalize().ok()?;
    let cfg = crate::config::CrapConfig::load(&config_dir).ok()?;
    let registry = crate::hooks::init_lua(&config_dir, &cfg).ok()?;
    let reg = registry.read().ok()?;
    let mut slugs: Vec<String> = reg.collections.keys().cloned().collect();
    slugs.sort();
    Some(slugs)
}

/// Try to load global slugs from the config dir for interactive selection.
pub fn try_load_global_slugs(config_dir: &Path) -> Option<Vec<String>> {
    let config_dir = config_dir.canonicalize().ok()?;
    let cfg = crate::config::CrapConfig::load(&config_dir).ok()?;
    let registry = crate::hooks::init_lua(&config_dir, &cfg).ok()?;
    let reg = registry.read().ok()?;
    let mut slugs: Vec<String> = reg.globals.keys().cloned().collect();
    slugs.sort();
    Some(slugs)
}

/// Try to load field names for a collection from the config dir.
pub fn try_load_field_names(config_dir: &Path, collection: &str) -> Option<Vec<String>> {
    let config_dir = config_dir.canonicalize().ok()?;
    let cfg = crate::config::CrapConfig::load(&config_dir).ok()?;
    let registry = crate::hooks::init_lua(&config_dir, &cfg).ok()?;
    let reg = registry.read().ok()?;
    let def = reg.get_collection(collection)?;
    Some(def.fields.iter().map(|f| f.name.clone()).collect())
}

/// Try to load field definitions (name + type + options) for condition hook scaffolding.
pub fn try_load_field_infos(config_dir: &Path, collection: &str) -> Option<Vec<crate::scaffold::ConditionFieldInfo>> {
    let config_dir = config_dir.canonicalize().ok()?;
    let cfg = crate::config::CrapConfig::load(&config_dir).ok()?;
    let registry = crate::hooks::init_lua(&config_dir, &cfg).ok()?;
    let reg = registry.read().ok()?;
    let def = reg.get_collection(collection)?;
    Some(def.fields.iter()
        .filter(|f| !matches!(f.field_type,
            crate::core::field::FieldType::Array
            | crate::core::field::FieldType::Blocks
            | crate::core::field::FieldType::Group
            | crate::core::field::FieldType::Row
            | crate::core::field::FieldType::Collapsible
            | crate::core::field::FieldType::Tabs
        ))
        .map(|f| crate::scaffold::ConditionFieldInfo {
            name: f.name.clone(),
            field_type: format!("{:?}", f.field_type).to_lowercase(),
            select_options: f.options.iter().map(|o| o.value.clone()).collect(),
        })
        .collect())
}
