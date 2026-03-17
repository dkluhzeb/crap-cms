//! `make` command — scaffold collections, globals, hooks, and jobs.

use anyhow::{Context as _, Result, anyhow, bail};
use std::path::Path;

use crate::{
    config::CrapConfig,
    core::{FieldType, SharedRegistry},
    hooks,
    scaffold::{self, CollectionOptions, ConditionFieldInfo, HookType, MakeHookOptions},
};

/// Dispatch the `make` subcommand.
#[cfg(not(tarpaulin_include))] // interactive dispatcher — uses dialoguer prompts
pub fn run(action: super::MakeAction) -> Result<()> {
    match action {
        super::MakeAction::Collection {
            config,
            slug,
            fields,
            no_timestamps,
            auth,
            upload,
            versions,
            no_input,
            force,
        } => {
            let opts = CollectionOptions {
                no_timestamps,
                auth,
                upload,
                versions,
                force,
            };
            make_collection_command(&config, slug, fields, !no_input, &opts)
        }
        super::MakeAction::Global {
            config,
            slug,
            fields,
            force,
        } => {
            let slug = match slug {
                Some(s) => s,
                None => {
                    use dialoguer::Input;
                    Input::<String>::new()
                        .with_prompt("Global slug")
                        .validate_with(|input: &String| -> Result<(), String> {
                            scaffold::validate_slug(input).map_err(|e| e.to_string())
                        })
                        .interact_text()
                        .context("Failed to read global slug")?
                }
            };
            {
                let parsed = fields
                    .map(|s| scaffold::parse_fields_shorthand(&s))
                    .transpose()?;
                scaffold::make_global(&config, &slug, parsed.as_deref(), force)
            }
        }
        super::MakeAction::Hook {
            config,
            name,
            hook_type,
            collection,
            position,
            field,
            force,
        } => make_hook_command(&config, name, hook_type, collection, position, field, force),
        super::MakeAction::Job {
            config,
            slug,
            schedule,
            queue,
            retries,
            timeout,
            force,
        } => {
            let slug = match slug {
                Some(s) => s,
                None => {
                    use dialoguer::Input;
                    Input::<String>::new()
                        .with_prompt("Job slug")
                        .validate_with(|input: &String| -> Result<(), String> {
                            scaffold::validate_slug(input).map_err(|e| e.to_string())
                        })
                        .interact_text()
                        .context("Failed to read job slug")?
                }
            };
            scaffold::make_job(
                &config,
                &slug,
                schedule.as_deref(),
                queue.as_deref(),
                retries,
                timeout,
                force,
            )
        }
    }
}

/// Handle the `make collection` subcommand — resolve missing args via interactive survey.
#[cfg(not(tarpaulin_include))] // uses dialoguer prompts throughout
pub(crate) fn make_collection_command(
    config_dir: &Path,
    slug: Option<String>,
    fields: Option<String>,
    interactive: bool,
    opts: &CollectionOptions,
) -> Result<()> {
    use dialoguer::{Confirm, Input};

    // 1. Resolve slug
    let slug = match slug {
        Some(s) => s,
        None if interactive => Input::<String>::new()
            .with_prompt("Collection slug")
            .validate_with(|input: &String| -> Result<(), String> {
                if input.is_empty() {
                    return Err("Slug cannot be empty".into());
                }
                if !input
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    return Err("Use lowercase letters, digits, and underscores only".into());
                }
                if input.starts_with('_') {
                    return Err("Slug cannot start with underscore".into());
                }
                Ok(())
            })
            .interact_text()
            .context("Failed to read collection slug")?,
        None => {
            bail!("Collection slug is required (or omit --no-input for interactive mode)")
        }
    };

    // 2. Resolve collection type flags (before fields, so the wizard can adapt)
    let auth = if opts.auth {
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

    let upload = if opts.upload {
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

    // 3. Resolve fields — survey when interactive and not provided via --fields
    let parsed_fields: Option<Vec<scaffold::FieldStub>> = match fields {
        Some(s) => Some(scaffold::parse_fields_shorthand(&s)?),
        None if interactive && (auth || upload) => {
            let hint = if auth {
                "email/password are included automatically"
            } else {
                "filename/mime_type/size are included automatically"
            };
            if Confirm::new()
                .with_prompt(format!("Add custom fields? ({})", hint))
                .default(false)
                .interact()
                .context("Failed to read custom fields preference")?
            {
                let f = scaffold::interactive_field_wizard(has_locales_enabled(config_dir))?;
                if f.is_empty() { None } else { Some(f) }
            } else {
                None
            }
        }
        None if interactive => {
            let f = scaffold::interactive_field_wizard(has_locales_enabled(config_dir))?;
            if f.is_empty() { None } else { Some(f) }
        }
        None => None, // non-interactive, use defaults
    };

    // 4. Resolve timestamps (only prompt in interactive mode)
    let no_timestamps = if opts.no_timestamps {
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

    // 5. Resolve versioning (only prompt in interactive mode)
    let versions = if opts.versions {
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

    let final_opts = CollectionOptions {
        no_timestamps,
        auth,
        upload,
        versions,
        force: opts.force,
    };
    scaffold::make_collection(config_dir, &slug, parsed_fields.as_deref(), &final_opts)
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
        Some(t) => HookType::from_name(&t).ok_or_else(|| {
            anyhow!(
                "Unknown hook type '{}' — valid: collection, field, access, condition",
                t
            )
        })?,
        None => {
            let items = &["Collection", "Field", "Access", "Condition"];
            let selection = Select::new()
                .with_prompt("Hook type")
                .items(items)
                .default(0)
                .interact()
                .context("Failed to read hook type selection")?;
            match selection {
                0 => HookType::Collection,
                1 => HookType::Field,
                2 => HookType::Access,
                _ => HookType::Condition,
            }
        }
    };

    // Load registry once for all interactive helpers
    let registry = try_load_registry(config_dir);

    // 2. Resolve collection/global — use registry for choices, fall back to text input
    let (collection, is_global) = match collection {
        Some(c) => {
            // Auto-detect: check if it's a global slug
            let is_global = registry
                .as_ref()
                .and_then(|r| r.read().ok())
                .map(|reg| reg.globals.contains_key(c.as_str()))
                .unwrap_or(false);
            (c, is_global)
        }
        None => {
            let (collection_slugs, global_slugs) = registry
                .as_ref()
                .and_then(|r| r.read().ok())
                .map(|reg| {
                    let mut cs: Vec<String> =
                        reg.collections.keys().map(|s| s.to_string()).collect();
                    cs.sort();
                    let mut gs: Vec<String> = reg.globals.keys().map(|s| s.to_string()).collect();
                    gs.sort();
                    (cs, gs)
                })
                .unwrap_or_default();

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
                bail!(
                    "Invalid position '{}' for {} hook — valid: {}",
                    p,
                    hook_type.label(),
                    hook_type.valid_positions().join(", ")
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
                let prompt = if hook_type == HookType::Condition {
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
    let field = if hook_type == HookType::Field {
        match field {
            Some(f) => Some(f),
            None => {
                let field_names: Option<Vec<String>> = registry
                    .as_ref()
                    .and_then(|r| r.read().ok())
                    .and_then(|reg| {
                        reg.get_collection(&collection)
                            .map(|def| def.fields.iter().map(|f| f.name.clone()).collect())
                    });

                if let Some(names) = field_names.filter(|n| !n.is_empty()) {
                    let selection = Select::new()
                        .with_prompt("Field")
                        .items(&names)
                        .default(0)
                        .interact()
                        .context("Failed to read field selection")?;
                    Some(names[selection].clone())
                } else {
                    Some(
                        Input::<String>::new()
                            .with_prompt("Field name")
                            .interact_text()
                            .context("Failed to read field name")?,
                    )
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
    let condition_field = if hook_type == HookType::Condition {
        let field_infos: Option<Vec<ConditionFieldInfo>> = registry
            .as_ref()
            .and_then(|r| r.read().ok())
            .and_then(|reg| {
                let def = reg.get_collection(&collection)?;
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
            });

        if let Some(ref f) = field {
            // CLI --field flag provided — look up type info from registry if available
            if let Some(ref infos) = field_infos {
                if let Some(info) = infos.iter().find(|i| i.name == *f) {
                    Some(info.clone())
                } else {
                    Some(ConditionFieldInfo {
                        name: f.clone(),
                        field_type: "text".to_string(),
                        select_options: vec![],
                    })
                }
            } else {
                Some(ConditionFieldInfo {
                    name: f.clone(),
                    field_type: "text".to_string(),
                    select_options: vec![],
                })
            }
        } else if let Some(infos) = field_infos.filter(|i| !i.is_empty()) {
            let labels: Vec<String> = infos
                .iter()
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

/// Check if localization is enabled in the config dir's crap.toml.
pub fn has_locales_enabled(config_dir: &Path) -> bool {
    CrapConfig::load(config_dir)
        .map(|cfg| cfg.locale.is_enabled())
        .unwrap_or(false)
}

/// Try to load the Lua registry once for reuse across make helpers.
pub fn try_load_registry(config_dir: &Path) -> Option<SharedRegistry> {
    let config_dir = config_dir.canonicalize().ok()?;
    let cfg = CrapConfig::load(&config_dir).ok()?;
    hooks::init_lua(&config_dir, &cfg).ok()
}

/// Try to load collection slugs from the config dir for interactive selection.
pub fn try_load_collection_slugs(config_dir: &Path) -> Option<Vec<String>> {
    let registry = try_load_registry(config_dir)?;
    let reg = registry.read().ok()?;
    let mut slugs: Vec<String> = reg.collections.keys().map(|s| s.to_string()).collect();
    slugs.sort();
    Some(slugs)
}

/// Try to load field names for a collection from the config dir.
pub fn try_load_field_names(config_dir: &Path, collection: &str) -> Option<Vec<String>> {
    let registry = try_load_registry(config_dir)?;
    let reg = registry.read().ok()?;
    let def = reg.get_collection(collection)?;
    Some(def.fields.iter().map(|f| f.name.clone()).collect())
}

/// Try to load field definitions (name + type + options) for condition hook scaffolding.
pub fn try_load_field_infos(
    config_dir: &Path,
    collection: &str,
) -> Option<Vec<ConditionFieldInfo>> {
    let registry = try_load_registry(config_dir)?;
    let reg = registry.read().ok()?;
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
