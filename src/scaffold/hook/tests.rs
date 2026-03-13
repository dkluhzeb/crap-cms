use std::{fs, path::Path};

use super::*;

fn make_hook_opts<'a>(
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

#[test]
fn test_make_hook_collection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "auto_slug",
        HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/auto_slug.lua")).unwrap();
    assert!(content.contains("before_change hook for posts"));
    assert!(
        content.contains("crap.hook.Posts"),
        "should use typed context, got:\n{content}"
    );
    assert!(
        !content.contains("crap.HookContext"),
        "should not use generic HookContext"
    );
    assert!(content.contains("return function(context)"));
}

#[test]
fn test_make_hook_collection_multi_word_slug() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "validate",
        HookType::Collection,
        "blog_posts",
        "before_validate",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/blog_posts/validate.lua")).unwrap();
    assert!(
        content.contains("crap.hook.BlogPosts"),
        "should PascalCase multi-word slug, got:\n{content}"
    );
}

#[test]
fn test_make_hook_global() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut opts = make_hook_opts(
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

    let content = fs::read_to_string(tmp.path().join("hooks/site_settings/on_change.lua")).unwrap();
    assert!(
        content.contains("crap.hook.global_site_settings"),
        "should use global hook type, got:\n{content}"
    );
    assert!(
        !content.contains("crap.hook.SiteSettings"),
        "should not use collection-style type"
    );
}

#[test]
fn test_make_hook_delete_uses_generic_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "cleanup",
        HookType::Collection,
        "posts",
        "before_delete",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/cleanup.lua")).unwrap();
    assert!(content.contains("before_delete hook for posts"));
    assert!(
        content.contains("crap.HookContext"),
        "delete hooks should use generic HookContext, got:\n{content}"
    );
    assert!(
        !content.contains("crap.hook.Posts"),
        "delete hooks should not use typed context"
    );
}

#[test]
fn test_make_hook_after_delete_uses_generic_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "notify",
        HookType::Collection,
        "posts",
        "after_delete",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/notify.lua")).unwrap();
    assert!(
        content.contains("crap.HookContext"),
        "after_delete should use generic HookContext"
    );
}

#[test]
fn test_make_hook_before_broadcast_uses_generic_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "filter_event",
        HookType::Collection,
        "posts",
        "before_broadcast",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/filter_event.lua")).unwrap();
    assert!(
        content.contains("crap.HookContext"),
        "before_broadcast should use generic HookContext, got:\n{content}"
    );
    assert!(
        !content.contains("crap.hook.Posts"),
        "before_broadcast should not use typed context"
    );
}

#[test]
fn test_make_hook_read_uses_typed_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "filter",
        HookType::Collection,
        "posts",
        "after_read",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/filter.lua")).unwrap();
    assert!(
        content.contains("crap.hook.Posts"),
        "read hooks should use typed context, got:\n{content}"
    );
}

#[test]
fn test_make_hook_field() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "normalize",
        HookType::Field,
        "posts",
        "before_validate",
        Some("title"),
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/normalize.lua")).unwrap();
    assert!(content.contains("before_validate field hook for posts.title"));
    assert!(
        content.contains("crap.field_hook.Posts"),
        "should use typed field hook context, got:\n{content}"
    );
    assert!(
        !content.contains("crap.FieldHookContext"),
        "should not use generic FieldHookContext"
    );
    assert!(content.contains("return function(value, context)"));
}

#[test]
fn test_make_hook_field_global() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut opts = make_hook_opts(
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

    let content = fs::read_to_string(tmp.path().join("hooks/site_settings/sanitize.lua")).unwrap();
    assert!(
        content.contains("crap.field_hook.global_site_settings"),
        "should use global field hook type, got:\n{content}"
    );
}

#[test]
fn test_make_hook_field_multi_word_slug() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "trim",
        HookType::Field,
        "blog_posts",
        "before_validate",
        Some("title"),
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/blog_posts/trim.lua")).unwrap();
    assert!(
        content.contains("crap.field_hook.BlogPosts"),
        "should PascalCase multi-word slug, got:\n{content}"
    );
}

#[test]
fn test_make_hook_access() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "admin_only",
        HookType::Access,
        "posts",
        "read",
        None,
        false,
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
        tmp.path(),
        "auto_slug",
        HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
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
        tmp.path(),
        "auto_slug",
        HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
    );
    make_hook(&opts).unwrap();
    let opts_force = make_hook_opts(
        tmp.path(),
        "auto_slug",
        HookType::Collection,
        "posts",
        "before_change",
        None,
        true,
    );
    assert!(make_hook(&opts_force).is_ok());
}

#[test]
fn test_make_hook_invalid_position() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "bad",
        HookType::Collection,
        "posts",
        "invalid_position",
        None,
        false,
    );
    let result = make_hook(&opts);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Invalid position"));
}

#[test]
fn test_make_hook_invalid_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "",
        HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
    );
    assert!(make_hook(&opts).is_err());

    let opts2 = make_hook_opts(
        tmp.path(),
        "bad-name",
        HookType::Collection,
        "posts",
        "before_change",
        None,
        false,
    );
    assert!(make_hook(&opts2).is_err());
}

#[test]
fn test_make_hook_condition_generic() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "show_url",
        HookType::Condition,
        "posts",
        "table",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/show_url.lua")).unwrap();
    assert!(content.contains("Display condition for posts (client-evaluated)"));
    assert!(
        content.contains("@param data crap.data.Posts"),
        "should use typed data, got:\n{content}"
    );
    assert!(content.contains("@return table"));
    assert!(content.contains("return function(data)"));
    // Generic template when no field info
    assert!(content.contains("field_name"));
}

#[test]
fn test_make_hook_condition_select() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut opts = make_hook_opts(
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

    let content = fs::read_to_string(tmp.path().join("hooks/posts/show_if_published.lua")).unwrap();
    assert!(content.contains(r#"field = "status""#));
    assert!(content.contains(r#"equals = "draft""#));
}

#[test]
fn test_make_hook_condition_checkbox() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut opts = make_hook_opts(
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

    let content = fs::read_to_string(tmp.path().join("hooks/posts/show_if_featured.lua")).unwrap();
    assert!(content.contains(r#"field = "is_featured""#));
    assert!(content.contains("is_truthy = true"));
}

#[test]
fn test_make_hook_condition_boolean() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut opts = make_hook_opts(
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
    assert!(
        content.contains("@param data crap.data.Posts"),
        "should use typed data, got:\n{content}"
    );
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
        tmp.path(),
        "show_if_count",
        HookType::Condition,
        "posts",
        "table",
        None,
        false,
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
        tmp.path(),
        "show_if_email",
        HookType::Condition,
        "posts",
        "table",
        None,
        false,
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
        tmp.path(),
        "show_if_sel",
        HookType::Condition,
        "posts",
        "table",
        None,
        false,
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

    let content = fs::read_to_string(tmp.path().join("hooks/site_settings/show_if.lua")).unwrap();
    assert!(
        content.contains("@param data crap.global_data.SiteSettings"),
        "should use global_data type, got:\n{content}"
    );
}

#[test]
fn test_make_hook_condition_boolean_no_field_info() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "bool_hook",
        HookType::Condition,
        "posts",
        "boolean",
        None,
        false,
    );
    make_hook(&opts).unwrap();

    let content = fs::read_to_string(tmp.path().join("hooks/posts/bool_hook.lua")).unwrap();
    assert!(content.contains("Display condition for posts (server-evaluated)"));
    assert!(content.contains("data.field_name"));
}

#[test]
fn test_hook_type_from_str() {
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
fn test_hook_type_label() {
    assert_eq!(HookType::Collection.label(), "collection");
    assert_eq!(HookType::Field.label(), "field");
    assert_eq!(HookType::Access.label(), "access");
    assert_eq!(HookType::Condition.label(), "condition");
}

#[test]
fn test_hook_type_valid_positions() {
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
    assert!(HookType::Condition.valid_positions().contains(&"boolean"));
}

#[test]
fn test_make_hook_invalid_collection_slug() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "hook",
        HookType::Collection,
        "Bad Slug",
        "before_change",
        None,
        false,
    );
    assert!(make_hook(&opts).is_err());
}

#[test]
fn test_make_hook_field_requires_field_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = make_hook_opts(
        tmp.path(),
        "hook",
        HookType::Field,
        "posts",
        "before_validate",
        None,
        false,
    );
    let result = make_hook(&opts);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("--field"));
}
