//! `make hook` — generate hook Lua files.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use crate::{cli, scaffold::render::render, typegen::to_pascal_case};

// ── Types ────────────────────────────────────────────────────────────────

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
    /// For condition hooks: info about the watched field.
    pub condition_field: Option<ConditionFieldInfo>,
    /// Whether the target is a global (vs collection).
    pub is_global: bool,
}

/// Field info used by condition hook scaffolding.
#[derive(Debug, Clone)]
pub struct ConditionFieldInfo {
    pub name: String,
    pub field_type: String,
    pub select_options: Vec<String>,
}

// ── Template rendering ───────────────────────────────────────────────────

/// Resolve the typed context annotation for collection/field hooks.
fn hook_context_type(collection: &str, is_global: bool, prefix: &str) -> String {
    if is_global {
        format!("crap.{prefix}.global_{collection}")
    } else {
        format!("crap.{prefix}.{}", to_pascal_case(collection))
    }
}

/// Return the typed data annotation for condition hooks.
fn condition_data_type(collection: &str, is_global: bool) -> String {
    let pascal = to_pascal_case(collection);

    if is_global {
        format!("crap.global_data.{pascal}")
    } else {
        format!("crap.data.{pascal}")
    }
}

/// Select the template name and build the context for rendering.
fn render_hook_lua(opts: &MakeHookOptions) -> Result<String> {
    match opts.hook_type {
        HookType::Collection => render_collection_hook(opts),
        HookType::Field => render_field_hook(opts),
        HookType::Access => render_access_hook(opts),
        HookType::Condition if opts.position == "boolean" => render_condition_boolean(opts),
        HookType::Condition => render_condition_table(opts),
    }
}

/// Render a collection hook.
fn render_collection_hook(opts: &MakeHookOptions) -> Result<String> {
    let is_generic = matches!(
        opts.position,
        "before_delete" | "after_delete" | "before_broadcast"
    );

    let context_type = if is_generic {
        "crap.HookContext".to_string()
    } else {
        hook_context_type(opts.collection, opts.is_global, "hook")
    };

    render(
        "hook_collection",
        &json!({
            "position": opts.position,
            "collection": opts.collection,
            "context_type": context_type,
        }),
    )
}

/// Render a field hook.
fn render_field_hook(opts: &MakeHookOptions) -> Result<String> {
    render(
        "hook_field",
        &json!({
            "position": opts.position,
            "collection": opts.collection,
            "field": opts.field.unwrap_or("?"),
            "context_type": hook_context_type(opts.collection, opts.is_global, "field_hook"),
        }),
    )
}

/// Render an access hook.
fn render_access_hook(opts: &MakeHookOptions) -> Result<String> {
    render(
        "hook_access",
        &json!({
            "position": opts.position,
            "collection": opts.collection,
        }),
    )
}

/// Render a boolean condition hook.
fn render_condition_boolean(opts: &MakeHookOptions) -> Result<String> {
    let field_name = opts
        .condition_field
        .as_ref()
        .map(|cf| cf.name.as_str())
        .unwrap_or("field_name");

    render(
        "hook_condition_boolean",
        &json!({
            "collection": opts.collection,
            "field_name": field_name,
            "data_type": condition_data_type(opts.collection, opts.is_global),
        }),
    )
}

/// Generate the condition body based on field type info.
fn condition_table_body(cf: &ConditionFieldInfo) -> String {
    match cf.field_type.as_str() {
        "select" if !cf.select_options.is_empty() => {
            format!(
                r#"    return {{ field = "{}", equals = "{}" }}"#,
                cf.name, cf.select_options[0]
            )
        }
        "checkbox" => format!(
            r#"    return {{ field = "{}", is_truthy = true }}"#,
            cf.name
        ),
        "number" => format!(
            r#"    return {{ field = "{}", not_equals = "0" }}"#,
            cf.name
        ),
        _ => format!(
            r#"    return {{ field = "{}", is_truthy = true }}"#,
            cf.name
        ),
    }
}

/// Render a table condition hook.
fn render_condition_table(opts: &MakeHookOptions) -> Result<String> {
    let body = if let Some(ref cf) = opts.condition_field {
        condition_table_body(cf)
    } else {
        "    -- TODO: replace \"field_name\" with the field to watch\n\n    return { field = \"field_name\", equals = \"value\" }".to_string()
    };

    render(
        "hook_condition_table",
        &json!({
            "collection": opts.collection,
            "data_type": condition_data_type(opts.collection, opts.is_global),
            "body": body,
        }),
    )
}

// ── Validation ───────────────────────────────────────────────────────────

/// Validate all inputs before generating the hook file.
fn validate_inputs(opts: &MakeHookOptions) -> Result<()> {
    crate::db::query::validate_slug(opts.collection)?;

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

    Ok(())
}

// ── Public entry point ───────────────────────────────────────────────────

/// Generate a hook file at `<config_dir>/hooks/<collection>/<name>.lua`.
pub fn make_hook(opts: &MakeHookOptions) -> Result<()> {
    validate_inputs(opts)?;

    let (hooks_dir, file_path) = if opts.hook_type == HookType::Access {
        let dir = opts.config_dir.join("access");
        (dir.clone(), dir.join(format!("{}.lua", opts.name)))
    } else {
        let dir = opts.config_dir.join("hooks").join(opts.collection);
        (dir.clone(), dir.join(format!("{}.lua", opts.name)))
    };

    fs::create_dir_all(&hooks_dir).context("Failed to create hook subdirectory")?;

    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = render_hook_lua(opts)?;

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    let hook_ref = if opts.hook_type == HookType::Access {
        format!("access.{}", opts.name)
    } else {
        format!("hooks.{}.{}", opts.collection, opts.name)
    };

    cli::success(&format!("Created {}", file_path.display()));
    cli::kv("Hook ref", &hook_ref);
    cli::hint(&integration_hint(opts, &hook_ref));

    Ok(())
}

/// Generate the integration hint shown after creating a hook.
fn integration_hint(opts: &MakeHookOptions, hook_ref: &str) -> String {
    match opts.hook_type {
        HookType::Collection => format!(
            "Add to your collection definition:\n  hooks = {{\n      {} = {{ \"{}\" }},\n  }},",
            opts.position, hook_ref
        ),
        HookType::Field => format!(
            "Add to your field definition:\n  hooks = {{\n      {} = {{ \"{}\" }},\n  }},",
            opts.position, hook_ref
        ),
        HookType::Access => format!(
            "Add to your collection definition:\n  access = {{\n      {} = \"{}\",\n  }},",
            opts.position, hook_ref
        ),
        HookType::Condition => format!(
            "Add to your field definition:\n  admin = {{\n      condition = \"{}\",\n  }},",
            hook_ref
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;

    /// Build a `MakeHookOptions` with common defaults for testing.
    fn make_opts<'a>(
        config_dir: &'a Path,
        name: &'a str,
        hook_type: HookType,
        collection: &'a str,
        position: &'a str,
        field: Option<&'a str>,
        force: bool,
    ) -> MakeHookOptions<'a> {
        MakeHookOptions {
            config_dir,
            name,
            hook_type,
            collection,
            position,
            field,
            force,
            condition_field: None,
            is_global: false,
        }
    }

    // ── HookType tests ──────────────────────────────────────────────────

    #[test]
    fn hook_type_from_str() {
        assert_eq!(
            HookType::from_name("collection"),
            Some(HookType::Collection)
        );
        assert_eq!(HookType::from_name("field"), Some(HookType::Field));
        assert_eq!(HookType::from_name("access"), Some(HookType::Access));
        assert_eq!(HookType::from_name("condition"), Some(HookType::Condition));
        assert_eq!(
            HookType::from_name("COLLECTION"),
            Some(HookType::Collection)
        );
        assert_eq!(HookType::from_name("unknown"), None);
    }

    #[test]
    fn hook_type_label() {
        assert_eq!(HookType::Collection.label(), "collection");
        assert_eq!(HookType::Field.label(), "field");
        assert_eq!(HookType::Access.label(), "access");
        assert_eq!(HookType::Condition.label(), "condition");
    }

    #[test]
    fn hook_type_valid_positions() {
        assert!(
            HookType::Collection
                .valid_positions()
                .contains(&"before_validate")
        );
        assert!(
            HookType::Collection
                .valid_positions()
                .contains(&"before_broadcast")
        );
        assert!(HookType::Field.valid_positions().contains(&"after_read"));
        assert!(HookType::Access.valid_positions().contains(&"read"));
        assert!(HookType::Condition.valid_positions().contains(&"table"));
    }

    // ── Validation ──────────────────────────────────────────────────────

    #[test]
    fn invalid_collection_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            make_hook(&make_opts(
                tmp.path(),
                "hook",
                HookType::Collection,
                "Bad Slug",
                "before_change",
                None,
                false
            ))
            .is_err()
        );
    }

    #[test]
    fn invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(
            make_hook(&make_opts(
                tmp.path(),
                "",
                HookType::Collection,
                "posts",
                "before_change",
                None,
                false
            ))
            .is_err()
        );
        assert!(
            make_hook(&make_opts(
                tmp.path(),
                "bad-name",
                HookType::Collection,
                "posts",
                "before_change",
                None,
                false
            ))
            .is_err()
        );
    }

    #[test]
    fn invalid_position() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = make_hook(&make_opts(
            tmp.path(),
            "bad",
            HookType::Collection,
            "posts",
            "invalid_position",
            None,
            false,
        ));
        assert!(result.unwrap_err().to_string().contains("Invalid position"));
    }

    #[test]
    fn field_requires_field_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = make_hook(&make_opts(
            tmp.path(),
            "hook",
            HookType::Field,
            "posts",
            "before_validate",
            None,
            false,
        ));
        assert!(result.unwrap_err().to_string().contains("--field"));
    }

    // ── Overwrite ───────────────────────────────────────────────────────

    #[test]
    fn refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = make_opts(
            tmp.path(),
            "auto_slug",
            HookType::Collection,
            "posts",
            "before_change",
            None,
            false,
        );
        make_hook(&opts).unwrap();
        assert!(
            make_hook(&opts)
                .unwrap_err()
                .to_string()
                .contains("--force")
        );
    }

    #[test]
    fn force_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "auto_slug",
            HookType::Collection,
            "posts",
            "before_change",
            None,
            false,
        ))
        .unwrap();
        assert!(
            make_hook(&make_opts(
                tmp.path(),
                "auto_slug",
                HookType::Collection,
                "posts",
                "before_change",
                None,
                true
            ))
            .is_ok()
        );
    }

    // ── Collection hooks ────────────────────────────────────────────────

    #[test]
    fn collection_hook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "auto_slug",
            HookType::Collection,
            "posts",
            "before_change",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/auto_slug.lua")).unwrap();
        assert!(content.contains("before_change hook for posts"));
        assert!(content.contains("crap.hook.Posts"));
        assert!(!content.contains("crap.HookContext"));
        assert!(content.contains("return function(context)"));
    }

    #[test]
    fn collection_hook_multi_word_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "validate",
            HookType::Collection,
            "blog_posts",
            "before_validate",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/blog_posts/validate.lua")).unwrap();
        assert!(content.contains("crap.hook.BlogPosts"));
    }

    #[test]
    fn collection_hook_global() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_opts(
            tmp.path(),
            "on_change",
            HookType::Collection,
            "site_settings",
            "before_change",
            None,
            false,
        );
        opts.is_global = true;
        make_hook(&opts).unwrap();
        let content =
            fs::read_to_string(tmp.path().join("hooks/site_settings/on_change.lua")).unwrap();
        assert!(content.contains("crap.hook.global_site_settings"));
    }

    #[test]
    fn delete_uses_generic_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "cleanup",
            HookType::Collection,
            "posts",
            "before_delete",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/cleanup.lua")).unwrap();
        assert!(content.contains("crap.HookContext"));
        assert!(!content.contains("crap.hook.Posts"));
    }

    #[test]
    fn after_delete_uses_generic_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "notify",
            HookType::Collection,
            "posts",
            "after_delete",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/notify.lua")).unwrap();
        assert!(content.contains("crap.HookContext"));
    }

    #[test]
    fn before_broadcast_uses_generic_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "filter_event",
            HookType::Collection,
            "posts",
            "before_broadcast",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/filter_event.lua")).unwrap();
        assert!(content.contains("crap.HookContext"));
    }

    #[test]
    fn read_uses_typed_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "filter",
            HookType::Collection,
            "posts",
            "after_read",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/filter.lua")).unwrap();
        assert!(content.contains("crap.hook.Posts"));
    }

    // ── Field hooks ─────────────────────────────────────────────────────

    #[test]
    fn field_hook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "normalize",
            HookType::Field,
            "posts",
            "before_validate",
            Some("title"),
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/normalize.lua")).unwrap();
        assert!(content.contains("before_validate field hook for posts.title"));
        assert!(content.contains("crap.field_hook.Posts"));
        assert!(content.contains("return function(value, context)"));
    }

    #[test]
    fn field_hook_global() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_opts(
            tmp.path(),
            "sanitize",
            HookType::Field,
            "site_settings",
            "before_change",
            Some("tagline"),
            false,
        );
        opts.is_global = true;
        make_hook(&opts).unwrap();
        let content =
            fs::read_to_string(tmp.path().join("hooks/site_settings/sanitize.lua")).unwrap();
        assert!(content.contains("crap.field_hook.global_site_settings"));
    }

    // ── Access hooks ────────────────────────────────────────────────────

    #[test]
    fn access_hook() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "admin_only",
            HookType::Access,
            "posts",
            "read",
            None,
            false,
        ))
        .unwrap();
        let file_path = tmp.path().join("access/admin_only.lua");
        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("read access control for posts"));
        assert!(content.contains("crap.AccessContext"));
    }

    // ── Condition hooks ─────────────────────────────────────────────────

    #[test]
    fn condition_generic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "show_url",
            HookType::Condition,
            "posts",
            "table",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_url.lua")).unwrap();
        assert!(content.contains("Display condition for posts (client-evaluated)"));
        assert!(content.contains("@param data crap.data.Posts"));
        assert!(content.contains("field_name"));
    }

    #[test]
    fn condition_select() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_opts(
            tmp.path(),
            "show_if_published",
            HookType::Condition,
            "posts",
            "table",
            None,
            false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "status".to_string(),
            field_type: "select".to_string(),
            select_options: vec!["draft".to_string(), "published".to_string()],
        });
        make_hook(&opts).unwrap();
        let content =
            fs::read_to_string(tmp.path().join("hooks/posts/show_if_published.lua")).unwrap();
        assert!(content.contains(r#"field = "status""#));
        assert!(content.contains(r#"equals = "draft""#));
    }

    #[test]
    fn condition_checkbox() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_opts(
            tmp.path(),
            "show_if_featured",
            HookType::Condition,
            "posts",
            "table",
            None,
            false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "is_featured".to_string(),
            field_type: "checkbox".to_string(),
            select_options: vec![],
        });
        make_hook(&opts).unwrap();
        let content =
            fs::read_to_string(tmp.path().join("hooks/posts/show_if_featured.lua")).unwrap();
        assert!(content.contains(r#"field = "is_featured""#));
        assert!(content.contains("is_truthy = true"));
    }

    #[test]
    fn condition_boolean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_opts(
            tmp.path(),
            "show_premium",
            HookType::Condition,
            "posts",
            "boolean",
            None,
            false,
        );
        opts.condition_field = Some(ConditionFieldInfo {
            name: "status".to_string(),
            field_type: "select".to_string(),
            select_options: vec!["draft".to_string(), "published".to_string()],
        });
        make_hook(&opts).unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/show_premium.lua")).unwrap();
        assert!(content.contains("Display condition for posts (server-evaluated)"));
        assert!(content.contains("@param data crap.data.Posts"));
        assert!(content.contains("data.status"));
    }

    #[test]
    fn condition_boolean_no_field_info() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_hook(&make_opts(
            tmp.path(),
            "bool_hook",
            HookType::Condition,
            "posts",
            "boolean",
            None,
            false,
        ))
        .unwrap();
        let content = fs::read_to_string(tmp.path().join("hooks/posts/bool_hook.lua")).unwrap();
        assert!(content.contains("data.field_name"));
    }

    #[test]
    fn condition_global_uses_global_data_type() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut opts = make_opts(
            tmp.path(),
            "show_if",
            HookType::Condition,
            "site_settings",
            "table",
            None,
            false,
        );
        opts.is_global = true;
        make_hook(&opts).unwrap();
        let content =
            fs::read_to_string(tmp.path().join("hooks/site_settings/show_if.lua")).unwrap();
        assert!(content.contains("@param data crap.global_data.SiteSettings"));
    }
}
