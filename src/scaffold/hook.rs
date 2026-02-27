//! `make hook` command — generate hook Lua files.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

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
    pub fn from_str(s: &str) -> Option<Self> {
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
                "before_validate", "before_change", "after_change",
                "before_read", "after_read",
                "before_delete", "after_delete", "before_broadcast",
            ],
            Self::Field => &[
                "before_validate", "before_change", "after_change", "after_read",
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

/// Generate a hook file at `<config_dir>/hooks/<collection>/<name>.lua`.
///
/// Creates a single-function file that returns the function directly (no module table).
/// The template varies by hook type (collection, field, or access).
pub fn make_hook(opts: &MakeHookOptions) -> Result<()> {
    // Validate inputs
    super::validate_slug(opts.collection)?;
    if opts.name.is_empty() || !opts.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        anyhow::bail!(
            "Invalid hook name '{}' — use alphanumeric characters and underscores only",
            opts.name
        );
    }
    if !opts.hook_type.valid_positions().contains(&opts.position) {
        anyhow::bail!(
            "Invalid position '{}' for {} hook — valid: {}",
            opts.position,
            opts.hook_type.label(),
            opts.hook_type.valid_positions().join(", ")
        );
    }
    if opts.hook_type == HookType::Field && opts.field.is_none() {
        anyhow::bail!("Field hooks require --field to be specified");
    }

    let hooks_dir = opts.config_dir.join("hooks").join(opts.collection);
    fs::create_dir_all(&hooks_dir)
        .context("Failed to create hooks/ subdirectory")?;

    let file_path = hooks_dir.join(format!("{}.lua", opts.name));
    if file_path.exists() && !opts.force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = match opts.hook_type {
        HookType::Collection => format!(
            r#"--- {position} hook for {collection}.
---@param context crap.HookContext
---@return crap.HookContext
return function(context)
    -- TODO: implement
    return context
end
"#,
            position = opts.position,
            collection = opts.collection,
        ),
        HookType::Field => format!(
            r#"--- {position} field hook for {collection}.{field}.
---@param value any
---@param context crap.FieldHookContext
---@return any
return function(value, context)
    -- TODO: implement
    return value
end
"#,
            position = opts.position,
            collection = opts.collection,
            field = opts.field.unwrap_or("?"),
        ),
        HookType::Access => format!(
            r#"--- {position} access control for {collection}.
---@param context crap.AccessContext
---@return boolean | table
return function(context)
    -- TODO: implement
    return true
end
"#,
            position = opts.position,
            collection = opts.collection,
        ),
        HookType::Condition if opts.position == "boolean" => {
            let field_name = opts.condition_field.as_ref()
                .map(|cf| cf.name.as_str())
                .unwrap_or("field_name");
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
---@param data table Current form field values.
---@return boolean
return function(data)
    -- TODO: implement
    local val = data.{field_name} or ""
    return val ~= ""
end
"#,
                collection = opts.collection,
                field_name = field_name,
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
    return { field = "field_name", equals = "value" }"#.to_string()
            };

            format!(
                r#"--- Display condition for {collection} (client-evaluated).
---
--- Returns a condition table — evaluated instantly in the browser,
--- no server round-trip. Prefer this over boolean returns when possible.
---
--- Condition table operators:
---   equals, not_equals, in, not_in, is_truthy, is_falsy
---   Array of conditions = AND (all must be true).
---
---@param data table Current form field values.
---@return table
return function(data)
{body}
end
"#,
                collection = opts.collection,
                body = body,
            )
        }
    };

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    let hook_ref = format!("hooks.{}.{}", opts.collection, opts.name);

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
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn make_hook_opts<'a>(
        config_dir: &'a Path,
        name: &'a str,
        hook_type: HookType,
        collection: &'a str,
        position: &'a str,
        field: Option<&'a str>,
        force: bool,
    ) -> MakeHookOptions<'a> {
        MakeHookOptions { config_dir, name, hook_type, collection, position, field, force, condition_field: None }
    }

    #[test]
    fn test_make_hook_collection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/auto_slug.lua")).unwrap();
        assert!(content.contains("before_change hook for posts"));
        assert!(content.contains("crap.HookContext"));
        assert!(content.contains("return function(context)"));
    }

    #[test]
    fn test_make_hook_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "normalize", HookType::Field,
            "posts", "before_validate", Some("title"), false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/normalize.lua")).unwrap();
        assert!(content.contains("before_validate field hook for posts.title"));
        assert!(content.contains("crap.FieldHookContext"));
        assert!(content.contains("return function(value, context)"));
    }

    #[test]
    fn test_make_hook_access() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "admin_only", HookType::Access,
            "posts", "read", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/admin_only.lua")).unwrap();
        assert!(content.contains("read access control for posts"));
        assert!(content.contains("crap.AccessContext"));
        assert!(content.contains("return true"));
    }

    #[test]
    fn test_make_hook_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, false,
        );
        make_hook(&opts).unwrap();
        let result = make_hook(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--force"));
    }

    #[test]
    fn test_make_hook_force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, false,
        );
        make_hook(&opts).unwrap();
        let opts_force = make_hook_opts(
            tmp.path(), "auto_slug", HookType::Collection,
            "posts", "before_change", None, true,
        );
        assert!(make_hook(&opts_force).is_ok());
    }

    #[test]
    fn test_make_hook_invalid_position() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "bad", HookType::Collection,
            "posts", "invalid_position", None, false,
        );
        let result = make_hook(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid position"));
    }

    #[test]
    fn test_make_hook_invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "", HookType::Collection,
            "posts", "before_change", None, false,
        );
        assert!(make_hook(&opts).is_err());

        let opts2 = make_hook_opts(
            tmp.path(), "bad-name", HookType::Collection,
            "posts", "before_change", None, false,
        );
        assert!(make_hook(&opts2).is_err());
    }

    #[test]
    fn test_make_hook_condition_generic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "show_url", HookType::Condition,
            "posts", "table", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_url.lua")).unwrap();
        assert!(content.contains("Display condition for posts (client-evaluated)"));
        assert!(content.contains("@return table"));
        assert!(content.contains("return function(data)"));
        // Generic template when no field info
        assert!(content.contains("field_name"));
    }

    #[test]
    fn test_make_hook_condition_select() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "show_if_published", HookType::Condition,
            "posts", "table", None, false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "status".to_string(),
            field_type: "select".to_string(),
            select_options: vec!["draft".to_string(), "published".to_string()],
        });
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_if_published.lua")).unwrap();
        assert!(content.contains(r#"field = "status""#));
        assert!(content.contains(r#"equals = "draft""#));
    }

    #[test]
    fn test_make_hook_condition_checkbox() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "show_if_featured", HookType::Condition,
            "posts", "table", None, false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "is_featured".to_string(),
            field_type: "checkbox".to_string(),
            select_options: vec![],
        });
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_if_featured.lua")).unwrap();
        assert!(content.contains(r#"field = "is_featured""#));
        assert!(content.contains("is_truthy = true"));
    }

    #[test]
    fn test_make_hook_condition_boolean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "show_premium", HookType::Condition,
            "posts", "boolean", None, false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "status".to_string(),
            field_type: "select".to_string(),
            select_options: vec!["draft".to_string(), "published".to_string()],
        });
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_premium.lua")).unwrap();
        assert!(content.contains("Display condition for posts (server-evaluated)"));
        assert!(content.contains("@return boolean"));
        assert!(content.contains("PERFORMANCE"));
        assert!(content.contains("server round-trip"));
        assert!(content.contains("return function(data)"));
        assert!(content.contains("data.status"));
    }

    #[test]
    fn test_make_hook_condition_number() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "show_if_count", HookType::Condition,
            "posts", "table", None, false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "count".to_string(),
            field_type: "number".to_string(),
            select_options: vec![],
        });
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_if_count.lua")).unwrap();
        assert!(content.contains(r#"field = "count""#));
        assert!(content.contains(r#"not_equals = "0""#));
    }

    #[test]
    fn test_make_hook_condition_text_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "show_if_email", HookType::Condition,
            "posts", "table", None, false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "email".to_string(),
            field_type: "email".to_string(),
            select_options: vec![],
        });
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_if_email.lua")).unwrap();
        assert!(content.contains(r#"field = "email""#));
        assert!(content.contains("is_truthy = true"));
    }

    #[test]
    fn test_make_hook_condition_select_empty_options() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "show_if_sel", HookType::Condition,
            "posts", "table", None, false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "status".to_string(),
            field_type: "select".to_string(),
            select_options: vec![], // empty options => falls through to default text-like template
        });
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_if_sel.lua")).unwrap();
        assert!(content.contains(r#"field = "status""#));
        assert!(content.contains("is_truthy = true"));
    }

    #[test]
    fn test_make_hook_condition_boolean_no_field_info() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "bool_hook", HookType::Condition,
            "posts", "boolean", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/bool_hook.lua")).unwrap();
        assert!(content.contains("Display condition for posts (server-evaluated)"));
        assert!(content.contains("data.field_name"));
    }

    #[test]
    fn test_hook_type_from_str() {
        assert_eq!(HookType::from_str("collection"), Some(HookType::Collection));
        assert_eq!(HookType::from_str("field"), Some(HookType::Field));
        assert_eq!(HookType::from_str("access"), Some(HookType::Access));
        assert_eq!(HookType::from_str("condition"), Some(HookType::Condition));
        assert_eq!(HookType::from_str("COLLECTION"), Some(HookType::Collection));
        assert_eq!(HookType::from_str("unknown"), None);
    }

    #[test]
    fn test_hook_type_label() {
        assert_eq!(HookType::Collection.label(), "collection");
        assert_eq!(HookType::Field.label(), "field");
        assert_eq!(HookType::Access.label(), "access");
        assert_eq!(HookType::Condition.label(), "condition");
    }

    #[test]
    fn test_hook_type_valid_positions() {
        assert!(HookType::Collection.valid_positions().contains(&"before_validate"));
        assert!(HookType::Collection.valid_positions().contains(&"before_broadcast"));
        assert!(HookType::Field.valid_positions().contains(&"after_read"));
        assert!(HookType::Access.valid_positions().contains(&"read"));
        assert!(HookType::Condition.valid_positions().contains(&"table"));
        assert!(HookType::Condition.valid_positions().contains(&"boolean"));
    }

    #[test]
    fn test_make_hook_invalid_collection_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "hook", HookType::Collection,
            "Bad Slug", "before_change", None, false,
        );
        assert!(make_hook(&opts).is_err());
    }

    #[test]
    fn test_make_hook_field_requires_field_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "hook", HookType::Field,
            "posts", "before_validate", None, false,
        );
        let result = make_hook(&opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--field"));
    }
}
