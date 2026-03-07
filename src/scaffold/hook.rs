//! `make hook` command — generate hook Lua files.

use anyhow::{Context as _, Result};
use std::fs;
use std::path::Path;

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

    let (hooks_dir, file_path) = if opts.hook_type == HookType::Access {
        let dir = opts.config_dir.join("access");
        let path = dir.join(format!("{}.lua", opts.name));
        (dir, path)
    } else {
        let dir = opts.config_dir.join("hooks").join(opts.collection);
        let path = dir.join(format!("{}.lua", opts.name));
        (dir, path)
    };
    fs::create_dir_all(&hooks_dir)
        .context("Failed to create hook subdirectory")?;
    if file_path.exists() && !opts.force {
        anyhow::bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = match opts.hook_type {
        HookType::Collection => {
            let is_generic = matches!(opts.position, "before_delete" | "after_delete" | "before_broadcast");
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
    -- TODO: implement
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
    -- TODO: implement
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
    -- TODO: implement
    local val = data.{field_name} or ""
    return val ~= ""
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
    return { field = "field_name", equals = "value" }"#.to_string()
            };

            let data_type = condition_data_type(opts.collection, opts.is_global);
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
        MakeHookOptions { config_dir, name, hook_type, collection, position, field, force, condition_field: None, is_global: false }
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
        assert!(content.contains("crap.hook.Posts"), "should use typed context, got:\n{content}");
        assert!(!content.contains("crap.HookContext"), "should not use generic HookContext");
        assert!(content.contains("return function(context)"));
    }

    #[test]
    fn test_make_hook_collection_multi_word_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "validate", HookType::Collection,
            "blog_posts", "before_validate", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/blog_posts/validate.lua")).unwrap();
        assert!(content.contains("crap.hook.BlogPosts"), "should PascalCase multi-word slug, got:\n{content}");
    }

    #[test]
    fn test_make_hook_global() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "on_change", HookType::Collection,
            "site_settings", "before_change", None, false,
        );
        opts.is_global = true;
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/site_settings/on_change.lua")).unwrap();
        assert!(content.contains("crap.hook.global_site_settings"), "should use global hook type, got:\n{content}");
        assert!(!content.contains("crap.hook.SiteSettings"), "should not use collection-style type");
    }

    #[test]
    fn test_make_hook_delete_uses_generic_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "cleanup", HookType::Collection,
            "posts", "before_delete", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/cleanup.lua")).unwrap();
        assert!(content.contains("before_delete hook for posts"));
        assert!(content.contains("crap.HookContext"), "delete hooks should use generic HookContext, got:\n{content}");
        assert!(!content.contains("crap.hook.Posts"), "delete hooks should not use typed context");
    }

    #[test]
    fn test_make_hook_after_delete_uses_generic_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "notify", HookType::Collection,
            "posts", "after_delete", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/notify.lua")).unwrap();
        assert!(content.contains("crap.HookContext"), "after_delete should use generic HookContext");
    }

    #[test]
    fn test_make_hook_before_broadcast_uses_generic_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "filter_event", HookType::Collection,
            "posts", "before_broadcast", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/filter_event.lua")).unwrap();
        assert!(content.contains("crap.HookContext"), "before_broadcast should use generic HookContext, got:\n{content}");
        assert!(!content.contains("crap.hook.Posts"), "before_broadcast should not use typed context");
    }

    #[test]
    fn test_make_hook_read_uses_typed_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "filter", HookType::Collection,
            "posts", "after_read", None, false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/posts/filter.lua")).unwrap();
        assert!(content.contains("crap.hook.Posts"), "read hooks should use typed context, got:\n{content}");
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
        assert!(content.contains("crap.field_hook.Posts"), "should use typed field hook context, got:\n{content}");
        assert!(!content.contains("crap.FieldHookContext"), "should not use generic FieldHookContext");
        assert!(content.contains("return function(value, context)"));
    }

    #[test]
    fn test_make_hook_field_global() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "sanitize", HookType::Field,
            "site_settings", "before_change", Some("tagline"), false,
        );
        opts.is_global = true;
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/site_settings/sanitize.lua")).unwrap();
        assert!(content.contains("crap.field_hook.global_site_settings"), "should use global field hook type, got:\n{content}");
    }

    #[test]
    fn test_make_hook_field_multi_word_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "trim", HookType::Field,
            "blog_posts", "before_validate", Some("title"), false,
        );
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/blog_posts/trim.lua")).unwrap();
        assert!(content.contains("crap.field_hook.BlogPosts"), "should PascalCase multi-word slug, got:\n{content}");
    }

    #[test]
    fn test_make_hook_access() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_hook_opts(
            tmp.path(), "admin_only", HookType::Access,
            "posts", "read", None, false,
        );
        make_hook(&opts).unwrap();

        // Access hooks go to access/ dir, not hooks/<collection>/
        let file_path = tmp.path().join("access/admin_only.lua");
        assert!(file_path.exists(), "access hook should be in access/ dir");
        let content = fs::read_to_string(&file_path).unwrap();
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
        assert!(content.contains("@param data crap.data.Posts"), "should use typed data, got:\n{content}");
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
        assert!(content.contains("@param data crap.data.Posts"), "should use typed data, got:\n{content}");
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
    fn test_make_hook_condition_global_uses_global_data_type() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_hook_opts(
            tmp.path(), "show_if", HookType::Condition,
            "site_settings", "table", None, false,
        );
        opts.is_global = true;
        make_hook(&opts).unwrap();

        let content = fs::read_to_string(tmp.path().join("hooks/site_settings/show_if.lua")).unwrap();
        assert!(content.contains("@param data crap.global_data.SiteSettings"), "should use global_data type, got:\n{content}");
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
