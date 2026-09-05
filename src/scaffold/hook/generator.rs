//! `make hook` -- generate hook Lua files.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use serde::Serialize;

use crate::{
    cli,
    hooks::lua_api::parse::{
        ACCESS_KEYS, COLLECTION_HOOK_KEYS, FIELD_HOOK_KEYS, GLOBAL_ACCESS_KEYS,
    },
    scaffold::{guards::refuse_file_overwrite, paths, render::render},
};

/// Handlebars context for the `hook_collection` template.
#[derive(Serialize)]
struct CollectionHookContext<'a> {
    position: &'a str,
    collection: &'a str,
    /// Full factory-call prefix up to the function literal, e.g.
    /// `crap.collections.posts.hook(` (typed),
    /// `crap.globals.settings.hook(` (global typed), or
    /// `crap.any.collection_hook(` (generic, used for
    /// `before_delete`/`after_delete`/`before_broadcast` where the
    /// runtime ships a `crap.HookContext` regardless of collection).
    factory_expr: String,
}

/// Handlebars context for the `hook_field` template.
#[derive(Serialize)]
struct FieldHookContext<'a> {
    position: &'a str,
    collection: &'a str,
    field: &'a str,
    /// Factory prefix — `crap.collections.posts.field_hook("title", `
    /// when the field is known (narrows `value` per field) or
    /// `crap.collections.posts.field_hook(` for the any-field form.
    factory_expr: String,
}

/// Handlebars context for the `hook_access` template.
#[derive(Serialize)]
struct AccessHookContext<'a> {
    position: &'a str,
    collection: &'a str,
}

/// Handlebars context for the `hook_condition_boolean` template.
#[derive(Serialize)]
struct ConditionBooleanContext<'a> {
    collection: &'a str,
    field_name: &'a str,
    /// `crap.collections.<slug>.condition(` /
    /// `crap.globals.<slug>.condition(`.
    factory_expr: String,
}

/// Handlebars context for the `hook_condition_table` template.
#[derive(Serialize)]
struct ConditionTableContext<'a> {
    collection: &'a str,
    factory_expr: String,
    body: String,
}

// == Types ================================================================

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
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "collection" => Some(Self::Collection),
            "field" => Some(Self::Field),
            "access" => Some(Self::Access),
            "condition" => Some(Self::Condition),
            _ => None,
        }
    }

    /// Valid lifecycle positions for this hook type. Collection, field,
    /// and access positions come straight from the parser's accepted-key
    /// constants — the scaffold can never drift from what the runtime
    /// actually accepts. Globals get the narrower access-key subset
    /// (`create`/`delete`/`trash`/`unlock` never fire on a single-row
    /// global and are rejected at load).
    #[must_use]
    pub fn valid_positions(&self, is_global: bool) -> &'static [&'static str] {
        match self {
            Self::Collection => COLLECTION_HOOK_KEYS,
            Self::Field => FIELD_HOOK_KEYS,
            Self::Access if is_global => GLOBAL_ACCESS_KEYS,
            Self::Access => ACCESS_KEYS,
            Self::Condition => &["table", "boolean"],
        }
    }

    /// Human-readable label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Collection => "collection",
            Self::Field => "field",
            Self::Access => "access",
            Self::Condition => "condition",
        }
    }
}

/// Options for `make_hook()`. Fully resolved -- no prompts.
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

// == Template rendering ===================================================

/// Build the factory-call prefix for a per-collection / per-global
/// typing helper — e.g. `crap.collections.posts.hook(` or
/// `crap.globals.site_settings.condition(`. The `method` is the
/// accessor method name (`hook`, `field_hook`, `condition`, …) and
/// `is_global` switches between the `crap.collections` and
/// `crap.globals` namespaces. The result is a string that, when
/// followed by `function(...)`, opens the factory wrapper.
fn factory_expr(collection: &str, is_global: bool, method: &str) -> String {
    if is_global {
        format!("crap.globals.{collection}.{method}(")
    } else {
        format!("crap.collections.{collection}.{method}(")
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
    // `before_delete` / `after_delete` / `before_broadcast` receive a
    // generic `crap.HookContext` because the runtime doesn't know which
    // collection's typed shape applies (delete carries only `{ id =
    // "..." }`; broadcast may run cross-collection). For those use
    // the generic `crap.any.collection_hook` factory; otherwise the
    // per-collection accessor narrows `ctx` per collection.
    let is_generic = matches!(
        opts.position,
        "before_delete" | "after_delete" | "before_broadcast"
    );
    let factory_expr = if is_generic {
        "crap.any.collection_hook(".to_string()
    } else {
        factory_expr(opts.collection, opts.is_global, "hook")
    };

    render(
        "hook_collection",
        &CollectionHookContext {
            position: opts.position,
            collection: opts.collection,
            factory_expr,
        },
    )
}

/// Render a field hook.
fn render_field_hook(opts: &MakeHookOptions) -> Result<String> {
    // Two factory shapes:
    //   - field known →
    //     `crap.collections.posts.field_hook("title", ` — narrows
    //     `value` to the field's declared type via the per-field
    //     overload.
    //   - field unknown (cross-field within a collection) →
    //     `crap.collections.posts.field_hook(` — single-arg form,
    //     `value` typed as `any`, `ctx` still typed per-collection.
    let base = factory_expr(opts.collection, opts.is_global, "field_hook");
    let expr = match opts.field {
        Some(name) => format!("{base}\"{name}\", "),
        None => base,
    };

    render(
        "hook_field",
        &FieldHookContext {
            position: opts.position,
            collection: opts.collection,
            field: opts.field.unwrap_or("?"),
            factory_expr: expr,
        },
    )
}

/// Render an access hook.
fn render_access_hook(opts: &MakeHookOptions) -> Result<String> {
    render(
        "hook_access",
        &AccessHookContext {
            position: opts.position,
            collection: opts.collection,
        },
    )
}

/// Render a boolean condition hook.
fn render_condition_boolean(opts: &MakeHookOptions) -> Result<String> {
    let field_name = opts
        .condition_field
        .as_ref()
        .map_or("field_name", |cf| cf.name.as_str());

    render(
        "hook_condition_boolean",
        &ConditionBooleanContext {
            collection: opts.collection,
            field_name,
            factory_expr: factory_expr(opts.collection, opts.is_global, "condition"),
        },
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
        "number" => format!(
            r#"    return {{ field = "{}", not_equals = "0" }}"#,
            cf.name
        ),
        // "checkbox" and anything else fall back to the truthy form.
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
        &ConditionTableContext {
            collection: opts.collection,
            factory_expr: factory_expr(opts.collection, opts.is_global, "condition"),
            body,
        },
    )
}

// == Validation ===========================================================

/// Validate all inputs before generating the hook file.
fn validate_inputs(opts: &MakeHookOptions) -> Result<()> {
    crate::db::query::validate_slug(opts.collection)?;

    if opts.name.is_empty() || !opts.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        bail!(
            "Invalid hook name '{}' -- use alphanumeric characters and underscores only",
            opts.name
        );
    }

    if !opts
        .hook_type
        .valid_positions(opts.is_global)
        .contains(&opts.position)
    {
        bail!(
            "Invalid position '{}' for {} hook -- valid: {}",
            opts.position,
            opts.hook_type.label(),
            opts.hook_type.valid_positions(opts.is_global).join(", ")
        );
    }

    if opts.hook_type == HookType::Field && opts.field.is_none() {
        bail!("Field hooks require --field to be specified");
    }

    Ok(())
}

// == Public entry point ===================================================

/// Generate a hook file at `<config_dir>/hooks/<collection>/<name>.lua`.
///
/// # Errors
///
/// Returns an error if any input is invalid, the file already exists without
/// `--force`, or writing fails.
pub fn make_hook(opts: &MakeHookOptions) -> Result<()> {
    validate_inputs(opts)?;

    let (hooks_dir, file_path) = if opts.hook_type == HookType::Access {
        let dir = paths::access_dir(opts.config_dir);
        (dir.clone(), dir.join(format!("{}.lua", opts.name)))
    } else {
        let dir = paths::collection_hooks_dir(opts.config_dir, opts.collection);
        (dir.clone(), dir.join(format!("{}.lua", opts.name)))
    };

    fs::create_dir_all(&hooks_dir).context("Failed to create hook subdirectory")?;

    refuse_file_overwrite(&file_path, opts.force)?;

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
            "Add to your field definition:\n  admin = {{\n      condition = \"{hook_ref}\",\n  }},"
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

    // == HookType tests ==================================================

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
                .valid_positions(false)
                .contains(&"before_validate")
        );
        assert!(
            HookType::Collection
                .valid_positions(false)
                .contains(&"before_broadcast")
        );
        assert!(
            HookType::Field
                .valid_positions(false)
                .contains(&"after_read")
        );
        assert!(
            HookType::Condition
                .valid_positions(false)
                .contains(&"table")
        );

        // Access offers every key the parser accepts on an `access`
        // sub-table — including the content-view keys (draft/trash/
        // versions) and the surface keys (unlock/admin/mcp).
        for key in [
            "read", "create", "update", "delete", "trash", "draft", "versions", "unlock", "admin",
            "mcp",
        ] {
            assert!(
                HookType::Access.valid_positions(false).contains(&key),
                "make hook access should offer '{key}'"
            );
        }

        // Globals get the narrower subset — the four keys the parser
        // rejects on a global are not offered.
        let global_keys = HookType::Access.valid_positions(true);
        for key in ["read", "draft", "update", "versions", "admin", "mcp"] {
            assert!(
                global_keys.contains(&key),
                "make hook access (global) should offer '{key}'"
            );
        }
        for key in ["create", "delete", "trash", "unlock"] {
            assert!(
                !global_keys.contains(&key),
                "make hook access (global) must not offer '{key}'"
            );
        }
    }

    // == Validation ======================================================

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

    // == Overwrite =======================================================

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

    // == Collection hooks ================================================

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
        assert!(content.contains("crap.collections.posts.hook("));
        assert!(!content.contains("crap.any.collection_hook"));
        assert!(!content.contains("crap.HookContext"));
        // Factory wraps the function literal — the open is on the same
        // line as `return crap.collections.posts.hook(`.
        assert!(content.contains("function(context)"));
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
        assert!(content.contains("crap.collections.blog_posts.hook("));
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
        assert!(content.contains("crap.globals.site_settings.hook("));
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
        assert!(content.contains("crap.any.collection_hook("));
        assert!(!content.contains("crap.collections.posts.hook("));
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
        assert!(content.contains("crap.any.collection_hook("));
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
        assert!(content.contains("crap.any.collection_hook("));
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
        assert!(content.contains("crap.collections.posts.hook("));
    }

    // == Field hooks =====================================================

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
        assert!(content.contains("crap.collections.posts.field_hook(\"title\", "));
        assert!(
            content.contains("return function(value, context)")
                || content.contains(", function(value, context)")
        );
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
        assert!(content.contains("crap.globals.site_settings.field_hook(\"tagline\", "));
    }

    // == Access hooks ====================================================

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
        assert!(content.contains("crap.any.access("));
    }

    // == Condition hooks =================================================

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
        assert!(content.contains("crap.collections.posts.condition("));
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
        assert!(content.contains("crap.collections.posts.condition("));
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
        assert!(content.contains("crap.globals.site_settings.condition("));
    }
}
