//! `make hook` command — generate hook Lua files.

use anyhow::{Context as _, Result, bail};
use std::{fs, path::Path};

use crate::typegen::to_pascal_case;

/// Hook type for the `make hook` command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HookType {
    Collection,
    Field,
    Access,
    Condition,
}

impl HookType {
    /// Parse from string (CLI input).
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "collection" => Some(Self::Collection),
            "field" => Some(Self::Field),
            "access" => Some(Self::Access),
            "condition" => Some(Self::Condition),
            _ => None,
        }
    }

    /// Valid lifecycle positions for this hook type.
    pub fn valid_positions(&self) -> &'static [&'static str] {
        match self {
            Self::Collection => &[
                "before_validate",
                "before_change",
                "after_change",
                "before_read",
                "after_read",
                "before_delete",
                "after_delete",
                "before_broadcast",
            ],
            Self::Field => &[
                "before_validate",
                "before_change",
                "after_change",
                "after_read",
            ],
            Self::Access => &["read", "create", "update", "delete"],
            Self::Condition => &["table", "boolean"],
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Collection => "collection",
            Self::Field => "field",
            Self::Access => "access",
            Self::Condition => "condition",
        }
    }
}

/// Options for `make_hook()`. Fully resolved — no prompts.
pub struct MakeHookOptions<'a> {
    pub config_dir: &'a Path,
    pub name: &'a str,
    pub hook_type: HookType,
    pub collection: &'a str,
    pub position: &'a str,
    pub field: Option<&'a str>,
    pub force: bool,
    /// For condition hooks: info about the watched field (used to generate
    /// a type-appropriate condition template).
    pub condition_field: Option<ConditionFieldInfo>,
    /// Whether the target is a global (vs collection). Controls the generated
    /// hook context type: `crap.hook.global_{slug}` vs `crap.hook.{PascalCase}`.
    pub is_global: bool,
}

/// Field info used by condition hook scaffolding to generate
/// type-appropriate condition templates.
#[derive(Debug, Clone)]
pub struct ConditionFieldInfo {
    /// The field name to watch (e.g., "status").
    pub name: String,
    /// The field type as a string (e.g., "select", "checkbox", "text").
    pub field_type: String,
    /// For select fields: the option values (e.g., ["draft", "published"]).
    pub select_options: Vec<String>,
}

/// Return the typed data annotation for condition hooks.
/// Collections use `crap.data.{PascalCase}`, globals use `crap.global_data.{PascalCase}`.
fn condition_data_type(collection: &str, is_global: bool) -> String {
    let pascal = to_pascal_case(collection);

    if is_global {
        format!("crap.global_data.{pascal}")
    } else {
        format!("crap.data.{pascal}")
    }
}

/// Generate a hook file at `<config_dir>/hooks/<collection>/<name>.lua`.
///
/// Creates a single-function file that returns the function directly (no module table).
/// The template varies by hook type (collection, field, or access).
pub fn make_hook(opts: &MakeHookOptions) -> Result<()> {
    // Validate inputs
    super::validate_slug(opts.collection)?;

    if opts.name.is_empty() || !opts.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        bail!(
            "Invalid hook name '{}' — use alphanumeric characters and underscores only",
            opts.name
        );
    }
    if !opts.hook_type.valid_positions().contains(&opts.position) {
        bail!(
            "Invalid position '{}' for {} hook — valid: {}",
            opts.position,
            opts.hook_type.label(),
            opts.hook_type.valid_positions().join(", ")
        );
    }
    if opts.hook_type == HookType::Field && opts.field.is_none() {
        bail!("Field hooks require --field to be specified");
    }

    let (hooks_dir, file_path) = if opts.hook_type == HookType::Access {
        let dir = opts.config_dir.join("access");
        let path = dir.join(format!("{}.lua", opts.name));
        (dir, path)
    } else {
        let dir = opts.config_dir.join("hooks").join(opts.collection);
        let path = dir.join(format!("{}.lua", opts.name));
        (dir, path)
    };
    fs::create_dir_all(&hooks_dir).context("Failed to create hook subdirectory")?;

    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = match opts.hook_type {
        HookType::Collection => {
            let is_generic = matches!(
                opts.position,
                "before_delete" | "after_delete" | "before_broadcast"
            );
            let context_type = if is_generic {
                // Delete and broadcast hooks use generic context (operation includes delete, not just create/update).
                "crap.HookContext".to_string()
            } else if opts.is_global {
                format!("crap.hook.global_{}", opts.collection)
            } else {
                format!("crap.hook.{}", to_pascal_case(opts.collection))
            };
            format!(
                r#"--- {position} hook for {collection}.
---@param context {context_type}
---@return {context_type}

return function(context)
    -- Example: context.data.title = string.upper(context.data.title)

    return context
end
"#,
                position = opts.position,
                collection = opts.collection,
                context_type = context_type,
            )
        }
        HookType::Field => {
            let context_type = if opts.is_global {
                format!("crap.field_hook.global_{}", opts.collection)
            } else {
                format!("crap.field_hook.{}", to_pascal_case(opts.collection))
            };
            format!(
                r#"--- {position} field hook for {collection}.{field}.
---@param value any
---@param context {context_type}
---@return any

return function(value, context)
    -- Example: return string.lower(value)

    return value
end
"#,
                position = opts.position,
                collection = opts.collection,
                field = opts.field.unwrap_or("?"),
                context_type = context_type,
            )
        }
        HookType::Access => format!(
            r#"--- {position} access control for {collection}.
---@param context crap.AccessContext
---@return boolean | table

return function(context)
    return true -- allow all (change to your logic)
end
"#,
            position = opts.position,
            collection = opts.collection,
        ),
        HookType::Condition if opts.position == "boolean" => {
            let field_name = opts
                .condition_field
                .as_ref()
                .map(|cf| cf.name.as_str())
                .unwrap_or("field_name");
            let data_type = condition_data_type(opts.collection, opts.is_global);
            format!(
                r#"--- Display condition for {collection} (server-evaluated).
---
--- Returns a boolean. Re-evaluated on the server via a debounced
--- fetch (300ms) whenever the user changes a form field.
---
--- PERFORMANCE: This makes a server round-trip on every change.
--- If your logic can be expressed as a simple comparison, prefer
--- returning a condition table instead (evaluated client-side, instant):
---   return {{ field = "{field_name}", equals = "value" }}
---
---@param data {data_type} Current form field values.
---@return boolean

return function(data)
    local val = data.{field_name} or ""
    return val ~= nil and val ~= ""
end
"#,
                collection = opts.collection,
                field_name = field_name,
                data_type = data_type,
            )
        }
        HookType::Condition => {
            let body = if let Some(ref cf) = opts.condition_field {
                match cf.field_type.as_str() {
                    "select" if !cf.select_options.is_empty() => {
                        format!(
                            r#"    return {{ field = "{name}", equals = "{val}" }}"#,
                            name = cf.name,
                            val = cf.select_options[0],
                        )
                    }
                    "checkbox" => {
                        format!(
                            r#"    return {{ field = "{name}", is_truthy = true }}"#,
                            name = cf.name,
                        )
                    }
                    "number" => {
                        format!(
                            r#"    return {{ field = "{name}", not_equals = "0" }}"#,
                            name = cf.name,
                        )
                    }
                    // text, textarea, email, richtext, relationship, upload, etc.
                    _ => {
                        format!(
                            r#"    return {{ field = "{name}", is_truthy = true }}"#,
                            name = cf.name,
                        )
                    }
                }
            } else {
                // No field info available — generic template
                r#"    -- TODO: replace "field_name" with the field to watch

    return { field = "field_name", equals = "value" }"#
                    .to_string()
            };

            let data_type = condition_data_type(opts.collection, opts.is_global);
            format!(
                r#"--- Display condition for {collection} (client-evaluated).
---
--- Returns a condition table -- evaluated instantly in the browser,
--- no server round-trip. Prefer this over boolean returns when possible.
---
--- Condition table operators:
---   equals, not_equals, in, not_in, is_truthy, is_falsy
---   Array of conditions = AND (all must be true).
---
---@param data {data_type} Current form field values.
---@return table

return function(data)
{body}
end
"#,
                collection = opts.collection,
                body = body,
                data_type = data_type,
            )
        }
    };

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    let hook_ref = if opts.hook_type == HookType::Access {
        format!("access.{}", opts.name)
    } else {
        format!("hooks.{}.{}", opts.collection, opts.name)
    };

    println!("Created {}", file_path.display());
    println!();
    println!("Hook ref: {}", hook_ref);
    println!();

    match opts.hook_type {
        HookType::Collection => {
            println!("Add to your collection definition:");
            println!("  hooks = {{");
            println!("      {} = {{ \"{}\" }},", opts.position, hook_ref);
            println!("  }},");
        }
        HookType::Field => {
            println!("Add to your field definition:");
            println!("  hooks = {{");
            println!("      {} = {{ \"{}\" }},", opts.position, hook_ref);
            println!("  }},");
        }
        HookType::Access => {
            println!("Add to your collection definition:");
            println!("  access = {{");
            println!("      {} = \"{}\",", opts.position, hook_ref);
            println!("  }},");
        }
        HookType::Condition => {
            println!("Add to your field definition:");
            println!("  admin = {{");
            println!("      condition = \"{}\",", hook_ref);
            println!("  }},");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
