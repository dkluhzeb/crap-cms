use std::{fs, path::Path};

use super::*;
use crate::{
    config::{CrapConfig, McpConfig},
    core::{
        CollectionDefinition, DocumentId, Registry, collection::GlobalDefinition,
        document::Document,
    },
    db::{migrate, pool, query},
    hooks::lifecycle::HookRunner,
};
use serde_json::{Map, Value, from_str, json};

fn make_registry() -> Registry {
    let mut reg = Registry::new();
    reg.register_collection(CollectionDefinition::new("posts"));
    reg.register_collection(CollectionDefinition::new("users"));
    reg.register_global(GlobalDefinition::new("settings"));
    reg
}

#[test]
fn generate_tools_basic() {
    let reg = make_registry();
    let config = McpConfig::default();
    let tools = generate_tools(&reg, &config);
    // 2 collections * 5 + 1 global * 2 + 4 introspection = 16
    assert!(tools.len() >= 16);
}

#[test]
fn exclude_collection() {
    let reg = make_registry();
    let config = McpConfig {
        exclude_collections: vec!["users".to_string()],
        ..Default::default()
    };
    let tools = generate_tools(&reg, &config);
    assert!(!tools.iter().any(|t| t.name.contains("users")));
    assert!(tools.iter().any(|t| t.name.contains("posts")));
}

#[test]
fn include_collection() {
    let reg = make_registry();
    let config = McpConfig {
        include_collections: vec!["posts".to_string()],
        ..Default::default()
    };
    let tools = generate_tools(&reg, &config);
    assert!(!tools.iter().any(|t| t.name.contains("users")));
    assert!(tools.iter().any(|t| t.name.contains("posts")));
}

#[test]
fn exclude_takes_precedence() {
    let reg = make_registry();
    let config = McpConfig {
        include_collections: vec!["posts".to_string(), "users".to_string()],
        exclude_collections: vec!["users".to_string()],
        ..Default::default()
    };
    let tools = generate_tools(&reg, &config);
    assert!(!tools.iter().any(|t| t.name.contains("users")));
}

#[test]
fn config_tools_included_when_enabled() {
    let reg = make_registry();
    let config = McpConfig {
        config_tools: true,
        ..Default::default()
    };
    let tools = generate_tools(&reg, &config);
    assert!(tools.iter().any(|t| t.name == "read_config_file"));
    assert!(tools.iter().any(|t| t.name == "write_config_file"));
    assert!(tools.iter().any(|t| t.name == "list_config_files"));
}

#[test]
fn config_tools_excluded_by_default() {
    let reg = make_registry();
    let config = McpConfig::default();
    let tools = generate_tools(&reg, &config);
    assert!(!tools.iter().any(|t| t.name == "read_config_file"));
}

#[test]
fn parse_tool_name_collection() {
    let reg = make_registry();
    let parsed = parse_tool_name("find_posts", &reg).unwrap();
    assert_eq!(parsed.op, ToolOp::Find);
    assert_eq!(parsed.slug, "posts");
}

#[test]
fn parse_tool_name_find_by_id() {
    let reg = make_registry();
    let parsed = parse_tool_name("find_by_id_posts", &reg).unwrap();
    assert_eq!(parsed.op, ToolOp::FindById);
    assert_eq!(parsed.slug, "posts");
}

#[test]
fn parse_tool_name_global() {
    let reg = make_registry();
    let parsed = parse_tool_name("global_read_settings", &reg).unwrap();
    assert_eq!(parsed.op, ToolOp::ReadGlobal);
    assert_eq!(parsed.slug, "settings");
}

#[test]
fn parse_tool_name_unknown() {
    let reg = make_registry();
    assert!(parse_tool_name("find_nonexistent", &reg).is_none());
}

#[test]
fn parse_tool_name_static() {
    let reg = make_registry();
    assert!(parse_tool_name("list_collections", &reg).is_none());
}

#[test]
fn global_tools_generated() {
    let reg = make_registry();
    let config = McpConfig::default();
    let tools = generate_tools(&reg, &config);
    assert!(tools.iter().any(|t| t.name == "global_read_settings"));
    assert!(tools.iter().any(|t| t.name == "global_update_settings"));
}

#[test]
fn introspection_tools_always_present() {
    let reg = Registry::new();
    let config = McpConfig::default();
    let tools = generate_tools(&reg, &config);
    assert!(tools.iter().any(|t| t.name == "list_collections"));
    assert!(tools.iter().any(|t| t.name == "describe_collection"));
    assert!(tools.iter().any(|t| t.name == "list_field_types"));
    assert!(tools.iter().any(|t| t.name == "cli_reference"));
}

#[test]
fn list_field_types_returns_all_types() {
    let result = exec_list_field_types().unwrap();
    let types: Vec<Value> = from_str(&result).unwrap();
    assert_eq!(types.len(), 20);

    // Verify all expected field types are present
    let names: Vec<&str> = types.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in &[
        "text",
        "number",
        "textarea",
        "select",
        "radio",
        "checkbox",
        "date",
        "email",
        "json",
        "richtext",
        "code",
        "relationship",
        "array",
        "group",
        "upload",
        "blocks",
        "row",
        "collapsible",
        "tabs",
        "join",
    ] {
        assert!(names.contains(expected), "Missing field type: {}", expected);
    }

    // Verify each entry has all required keys
    for t in &types {
        assert!(t.get("name").is_some());
        assert!(t.get("description").is_some());
        assert!(t.get("json_schema_type").is_some());
        assert!(t.get("supports_has_many").is_some());
        assert!(t.get("supports_sub_fields").is_some());
        assert!(t.get("supports_options").is_some());
    }
}

#[test]
fn cli_reference_all_commands() {
    let result = exec_cli_reference(&json!({})).unwrap();
    let parsed: Value = from_str(&result).unwrap();
    let commands = parsed["commands"].as_array().unwrap();
    assert!(commands.len() >= 15);

    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for expected in &["serve", "migrate", "user", "backup", "jobs", "mcp"] {
        assert!(names.contains(expected), "Missing command: {}", expected);
    }
}

#[test]
fn cli_reference_specific_command() {
    let result = exec_cli_reference(&json!({ "command": "migrate" })).unwrap();
    let parsed: Value = from_str(&result).unwrap();
    assert!(parsed.get("subcommands").is_some());
    let subs = parsed["subcommands"].as_array().unwrap();
    let sub_names: Vec<&str> = subs.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert!(sub_names.contains(&"up"));
    assert!(sub_names.contains(&"down"));
    assert!(sub_names.contains(&"create"));
    assert!(sub_names.contains(&"list"));
    assert!(sub_names.contains(&"fresh"));
}

#[test]
fn cli_reference_unknown_command() {
    let result = exec_cli_reference(&json!({ "command": "nonexistent" })).unwrap();
    let parsed: Value = from_str(&result).unwrap();
    assert!(parsed.get("error").is_some());
}

#[test]
fn safe_config_path_rejects_absolute() {
    let dir = Path::new("/tmp");
    assert!(safe_config_path(dir, "/etc/passwd").is_err());
}

#[test]
fn safe_config_path_rejects_dot_dot() {
    let dir = Path::new("/tmp");
    assert!(safe_config_path(dir, "../etc/passwd").is_err());
    assert!(safe_config_path(dir, "foo/../../etc/passwd").is_err());
}

#[test]
fn safe_config_path_allows_relative() {
    let dir = std::env::temp_dir();
    // Should succeed — a simple relative path within an existing dir
    let result = safe_config_path(&dir, "test_file.txt");
    assert!(result.is_ok());
}

// config_tools enforcement is tested via generate_tools (excluded by default)
// and the match guard in execute_tool. The guard is purely a config check before
// any DB/hook access, verified by code inspection. Integration tests cover e2e.

#[test]
fn parse_where_in_operator() {
    let args = json!({
        "where": {
            "status": { "in": ["draft", "review"] }
        }
    });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert_eq!(f.field, "status");
            match &f.op {
                query::FilterOp::In(vals) => assert_eq!(vals, &["draft", "review"]),
                other => panic!("Expected In, got {:?}", other),
            }
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_not_in_operator() {
    let args = json!({
        "where": {
            "role": { "not_in": ["banned", "suspended"] }
        }
    });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert_eq!(f.field, "role");
            assert!(matches!(&f.op, query::FilterOp::NotIn(_)));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_exists_operator() {
    let args = json!({
        "where": {
            "avatar": { "exists": true }
        }
    });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert_eq!(f.field, "avatar");
            assert!(matches!(&f.op, query::FilterOp::Exists));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_not_exists_operator() {
    let args = json!({
        "where": {
            "deleted_at": { "not_exists": true }
        }
    });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert!(matches!(&f.op, query::FilterOp::NotExists));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

// ── parse_where_filters: scalar field values ───────────────────────────

#[test]
fn parse_where_string_shorthand() {
    // { "field": "value" } → Equals
    let args = json!({ "where": { "title": "hello" } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert_eq!(f.field, "title");
            assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "hello"));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_number_shorthand() {
    let args = json!({ "where": { "count": 5 } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert_eq!(f.field, "count");
            assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "5"));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_bool_shorthand_true() {
    let args = json!({ "where": { "active": true } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "1"));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_bool_shorthand_false() {
    let args = json!({ "where": { "active": false } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "0"));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_scalar_operators() {
    for (op_name, expected_variant) in &[
        ("not_equals", "not_equals"),
        ("contains", "contains"),
        ("greater_than", "greater_than"),
        ("greater_than_equal", "greater_than_equal"),
        ("less_than", "less_than"),
        ("less_than_equal", "less_than_equal"),
        ("like", "like"),
    ] {
        let args = {
            let mut where_field = Map::new();
            where_field.insert(op_name.to_string(), json!("val"));
            let mut where_obj = Map::new();
            where_obj.insert("field".to_string(), Value::Object(where_field));
            let mut root = Map::new();
            root.insert("where".to_string(), Value::Object(where_obj));
            Value::Object(root)
        };
        let clauses = parse_where_filters(&args);
        assert_eq!(
            clauses.len(),
            1,
            "operator {} produced wrong clause count",
            op_name
        );
        match &clauses[0] {
            query::FilterClause::Single(f) => match (&f.op, *expected_variant) {
                (query::FilterOp::NotEquals(_), "not_equals") => {}
                (query::FilterOp::Contains(_), "contains") => {}
                (query::FilterOp::GreaterThan(_), "greater_than") => {}
                (query::FilterOp::GreaterThanOrEqual(_), "greater_than_equal") => {}
                (query::FilterOp::LessThan(_), "less_than") => {}
                (query::FilterOp::LessThanOrEqual(_), "less_than_equal") => {}
                (query::FilterOp::Like(_), "like") => {}
                _ => panic!("Wrong op variant for operator {}: got {:?}", op_name, f.op),
            },
            other => panic!("Expected Single for {}, got {:?}", op_name, other),
        }
    }
}

#[test]
fn parse_where_scalar_op_with_number() {
    let args = json!({ "where": { "age": { "greater_than": 18 } } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert!(matches!(&f.op, query::FilterOp::GreaterThan(v) if v == "18"));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_scalar_op_with_bool() {
    let args = json!({ "where": { "active": { "equals": true } } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 1);
    match &clauses[0] {
        query::FilterClause::Single(f) => {
            assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "1"));
        }
        other => panic!("Expected Single, got {:?}", other),
    }
}

#[test]
fn parse_where_unknown_op_skipped() {
    // Unknown operator name → clause is skipped (no panic)
    let args = json!({ "where": { "field": { "unknown_op": "val" } } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 0);
}

#[test]
fn parse_where_null_value_skipped() {
    // Null field value → skipped
    let args = json!({ "where": { "field": null } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 0);
}

#[test]
fn parse_where_null_op_value_skipped() {
    // Op value is null → skipped (neither scalar nor array)
    let args = json!({ "where": { "field": { "equals": null } } });
    let clauses = parse_where_filters(&args);
    assert_eq!(clauses.len(), 0);
}

#[test]
fn parse_where_no_where_key() {
    let args = json!({ "limit": 10 });
    let clauses = parse_where_filters(&args);
    assert!(clauses.is_empty());
}

#[test]
fn parse_where_non_object_where() {
    let args = json!({ "where": "not-an-object" });
    let clauses = parse_where_filters(&args);
    assert!(clauses.is_empty());
}

// ── exec_list_collections ──────────────────────────────────────────────

#[test]
fn exec_list_collections_returns_all() {
    let reg = make_registry();
    let config = McpConfig::default();
    let result = super::exec_list_collections(&reg, &config).unwrap();
    let items: Vec<Value> = from_str(&result).unwrap();
    // posts, users (collections) + settings (global)
    assert!(items.len() >= 3);
    let slugs: Vec<&str> = items
        .iter()
        .map(|i| i["slug"].as_str().unwrap_or(""))
        .collect();
    assert!(slugs.contains(&"posts"));
    assert!(slugs.contains(&"settings"));
}

#[test]
fn exec_list_collections_respects_exclude() {
    let reg = make_registry();
    let config = McpConfig {
        exclude_collections: vec!["users".to_string()],
        ..Default::default()
    };
    let result = super::exec_list_collections(&reg, &config).unwrap();
    let items: Vec<Value> = from_str(&result).unwrap();
    let slugs: Vec<&str> = items
        .iter()
        .map(|i| i["slug"].as_str().unwrap_or(""))
        .collect();
    assert!(!slugs.contains(&"users"));
    assert!(slugs.contains(&"posts"));
}

#[test]
fn exec_list_collections_empty_registry() {
    let reg = Registry::new();
    let config = McpConfig::default();
    let result = super::exec_list_collections(&reg, &config).unwrap();
    let items: Vec<Value> = from_str(&result).unwrap();
    assert!(items.is_empty());
}

// ── exec_describe_collection ───────────────────────────────────────────

#[test]
fn exec_describe_collection_for_collection() {
    let reg = make_registry();
    let config = McpConfig::default();
    let args = json!({ "slug": "posts" });
    let result = super::exec_describe_collection(&args, &reg, &config).unwrap();
    let parsed: Value = from_str(&result).unwrap();
    assert_eq!(parsed["slug"], "posts");
    assert_eq!(parsed["type"], "collection");
    assert!(parsed["schema"].is_object());
}

#[test]
fn exec_describe_collection_for_global() {
    let reg = make_registry();
    let config = McpConfig::default();
    let args = json!({ "slug": "settings" });
    let result = super::exec_describe_collection(&args, &reg, &config).unwrap();
    let parsed: Value = from_str(&result).unwrap();
    assert_eq!(parsed["slug"], "settings");
    assert_eq!(parsed["type"], "global");
    assert!(parsed["schema"].is_object());
}

#[test]
fn exec_describe_collection_unknown_slug_errors() {
    let reg = make_registry();
    let config = McpConfig::default();
    let args = json!({ "slug": "nonexistent" });
    let err = super::exec_describe_collection(&args, &reg, &config).unwrap_err();
    assert!(err.to_string().contains("Unknown"));
}

#[test]
fn exec_describe_collection_missing_slug_errors() {
    let reg = make_registry();
    let config = McpConfig::default();
    let args = json!({});
    let err = super::exec_describe_collection(&args, &reg, &config).unwrap_err();
    assert!(err.to_string().contains("slug"));
}

#[test]
fn exec_describe_collection_excluded_errors() {
    let reg = make_registry();
    let config = McpConfig {
        exclude_collections: vec!["posts".to_string()],
        ..Default::default()
    };
    let args = json!({ "slug": "posts" });
    let err = super::exec_describe_collection(&args, &reg, &config).unwrap_err();
    assert!(err.to_string().contains("Unknown"));
}

// ── config file tools ──────────────────────────────────────────────────

#[test]
fn exec_read_config_file_success() {
    let dir = tempfile::tempdir().unwrap();
    // Write a test file
    fs::write(dir.path().join("hello.txt"), "world").unwrap();
    let args = json!({ "path": "hello.txt" });
    let result = super::exec_read_config_file(&args, dir.path()).unwrap();
    assert_eq!(result, "world");
}

#[test]
fn exec_read_config_file_missing_path_arg_errors() {
    let dir = tempfile::tempdir().unwrap();
    let args = json!({});
    let err = super::exec_read_config_file(&args, dir.path()).unwrap_err();
    assert!(err.to_string().contains("path"));
}

#[test]
fn exec_read_config_file_nonexistent_file_errors() {
    let dir = tempfile::tempdir().unwrap();
    let args = json!({ "path": "does_not_exist.txt" });
    let err = super::exec_read_config_file(&args, dir.path()).unwrap_err();
    assert!(err.to_string().contains("does_not_exist"));
}

#[test]
fn exec_write_config_file_success() {
    let dir = tempfile::tempdir().unwrap();
    let args = json!({ "path": "output.txt", "content": "hello" });
    let result = super::exec_write_config_file(&args, dir.path()).unwrap();
    let parsed: Value = from_str(&result).unwrap();
    assert_eq!(parsed["written"], "output.txt");
    let written = fs::read_to_string(dir.path().join("output.txt")).unwrap();
    assert_eq!(written, "hello");
}

#[test]
fn exec_write_config_file_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let args = json!({ "path": "subdir/nested/file.txt", "content": "data" });
    let result = super::exec_write_config_file(&args, dir.path()).unwrap();
    let parsed: Value = from_str(&result).unwrap();
    assert_eq!(parsed["written"], "subdir/nested/file.txt");
    let content = fs::read_to_string(dir.path().join("subdir/nested/file.txt")).unwrap();
    assert_eq!(content, "data");
}

#[test]
fn exec_write_config_file_missing_path_errors() {
    let dir = tempfile::tempdir().unwrap();
    let args = json!({ "content": "data" });
    let err = super::exec_write_config_file(&args, dir.path()).unwrap_err();
    assert!(err.to_string().contains("path"));
}

#[test]
fn exec_write_config_file_missing_content_errors() {
    let dir = tempfile::tempdir().unwrap();
    let args = json!({ "path": "file.txt" });
    let err = super::exec_write_config_file(&args, dir.path()).unwrap_err();
    assert!(err.to_string().contains("content"));
}

#[test]
fn exec_list_config_files_root() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "").unwrap();
    fs::write(dir.path().join("b.lua"), "").unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();

    let args = json!({});
    let result = super::exec_list_config_files(&args, dir.path()).unwrap();
    let files: Vec<Value> = from_str(&result).unwrap();
    assert!(files.len() >= 3);
    let names: Vec<&str> = files
        .iter()
        .map(|f| f["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"b.lua"));
    assert!(names.contains(&"sub"));
    // Check types
    let sub = files.iter().find(|f| f["name"] == "sub").unwrap();
    assert_eq!(sub["type"], "directory");
    let a = files.iter().find(|f| f["name"] == "a.txt").unwrap();
    assert_eq!(a["type"], "file");
}

#[test]
fn exec_list_config_files_subdirectory() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("collections")).unwrap();
    fs::write(dir.path().join("collections/posts.lua"), "").unwrap();

    let args = json!({ "path": "collections" });
    let result = super::exec_list_config_files(&args, dir.path()).unwrap();
    let files: Vec<Value> = from_str(&result).unwrap();
    let names: Vec<&str> = files
        .iter()
        .map(|f| f["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.contains(&"posts.lua"));
}

#[test]
fn exec_list_config_files_nonexistent_dir_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    // Subdir does not exist → safe_config_path succeeds (no traversal),
    // but the dir is not a directory so files is empty
    let args = json!({ "path": "nonexistent" });
    let result = super::exec_list_config_files(&args, dir.path()).unwrap();
    let files: Vec<Value> = from_str(&result).unwrap();
    assert!(files.is_empty());
}

// ── doc_to_json ────────────────────────────────────────────────────────

#[test]
fn doc_to_json_includes_all_fields() {
    use std::collections::HashMap;
    let mut fields = HashMap::new();
    fields.insert("title".to_string(), json!("Hello"));
    fields.insert("count".to_string(), json!(42));
    let doc = Document {
        id: DocumentId::new("abc123"),
        fields,
        created_at: Some("2024-01-01T00:00:00Z".to_string()),
        updated_at: Some("2024-06-01T00:00:00Z".to_string()),
    };
    let val = super::doc_to_json(&doc);
    assert_eq!(val["id"], "abc123");
    assert_eq!(val["title"], "Hello");
    assert_eq!(val["count"], 42);
    assert_eq!(val["created_at"], "2024-01-01T00:00:00Z");
    assert_eq!(val["updated_at"], "2024-06-01T00:00:00Z");
}

#[test]
fn doc_to_json_without_timestamps() {
    use std::collections::HashMap;
    let doc = Document {
        id: DocumentId::new("xyz"),
        fields: HashMap::new(),
        created_at: None,
        updated_at: None,
    };
    let val = super::doc_to_json(&doc);
    assert_eq!(val["id"], "xyz");
    assert!(val.get("created_at").is_none() || val["created_at"].is_null());
    assert!(val.get("updated_at").is_none() || val["updated_at"].is_null());
}

// ── execute_tool: config_tools guard ──────────────────────────────────

#[test]
fn execute_tool_config_tools_disabled_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    // config_tools is false by default
    assert!(!config.mcp.config_tools);

    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    let shared = Registry::shared();
    migrate::sync_all(&db_pool, &shared, &config.locale).unwrap();
    let registry = Registry::snapshot(&shared);
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(shared)
        .config(&config)
        .build()
        .unwrap();

    let err = execute_tool(
        "read_config_file",
        &json!({ "path": "init.lua" }),
        &db_pool,
        &registry,
        &runner,
        tmp.path(),
        &config,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("config_tools"));
}

#[test]
fn execute_tool_unknown_tool_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    let shared = Registry::shared();
    migrate::sync_all(&db_pool, &shared, &config.locale).unwrap();
    let registry = Registry::snapshot(&shared);
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(shared)
        .config(&config)
        .build()
        .unwrap();

    let err = execute_tool(
        "completely_unknown",
        &json!({}),
        &db_pool,
        &registry,
        &runner,
        tmp.path(),
        &config,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Unknown tool"));
}

// ── execute_tool: exclude_collections enforcement ─────────────────────

#[test]
fn execute_tool_excluded_collection_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.mcp.exclude_collections = vec!["posts".to_string()];

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(CollectionDefinition::new("posts"));
    }

    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &shared, &config.locale).unwrap();
    let registry = Registry::snapshot(&shared);
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(shared)
        .config(&config)
        .build()
        .unwrap();

    // An attacker who knows the slug "posts" tries to call find_posts directly
    let err = execute_tool(
        "find_posts",
        &json!({ "limit": 10 }),
        &db_pool,
        &registry,
        &runner,
        tmp.path(),
        &config,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("Tool not available"),
        "Expected 'Tool not available' error, got: {}",
        err
    );
}

#[test]
fn execute_tool_included_collection_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    // Only include "posts", exclude everything else implicitly
    config.mcp.include_collections = vec!["posts".to_string()];

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(CollectionDefinition::new("posts"));
        reg.register_collection(CollectionDefinition::new("users"));
    }

    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &shared, &config.locale).unwrap();
    let registry = Registry::snapshot(&shared);
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(shared)
        .config(&config)
        .build()
        .unwrap();

    // find_posts should work (included)
    let result = execute_tool(
        "find_posts",
        &json!({}),
        &db_pool,
        &registry,
        &runner,
        tmp.path(),
        &config,
        None,
        None,
        None,
    );
    assert!(result.is_ok(), "find_posts should succeed: {:?}", result);

    // find_users should be blocked (not in include list)
    let err = execute_tool(
        "find_users",
        &json!({}),
        &db_pool,
        &registry,
        &runner,
        tmp.path(),
        &config,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("Tool not available"),
        "Expected 'Tool not available' error for users, got: {}",
        err
    );
}

// ── parse_tool_name: update and delete ops ────────────────────────────

#[test]
fn parse_tool_name_create() {
    let reg = make_registry();
    let parsed = parse_tool_name("create_posts", &reg).unwrap();
    assert_eq!(parsed.op, ToolOp::Create);
    assert_eq!(parsed.slug, "posts");
}

#[test]
fn parse_tool_name_update() {
    let reg = make_registry();
    let parsed = parse_tool_name("update_posts", &reg).unwrap();
    assert_eq!(parsed.op, ToolOp::Update);
    assert_eq!(parsed.slug, "posts");
}

#[test]
fn parse_tool_name_delete() {
    let reg = make_registry();
    let parsed = parse_tool_name("delete_posts", &reg).unwrap();
    assert_eq!(parsed.op, ToolOp::Delete);
    assert_eq!(parsed.slug, "posts");
}

#[test]
fn parse_tool_name_global_update() {
    let reg = make_registry();
    let parsed = parse_tool_name("global_update_settings", &reg).unwrap();
    assert_eq!(parsed.op, ToolOp::UpdateGlobal);
    assert_eq!(parsed.slug, "settings");
}

#[test]
fn should_include_basic() {
    let config = McpConfig::default();
    assert!(should_include("posts", &config));
    assert!(should_include("users", &config));
}

#[test]
fn should_include_with_include_list() {
    let config = McpConfig {
        include_collections: vec!["posts".to_string()],
        ..Default::default()
    };
    assert!(should_include("posts", &config));
    assert!(!should_include("users", &config));
}

#[test]
fn should_include_with_exclude_list() {
    let config = McpConfig {
        exclude_collections: vec!["users".to_string()],
        ..Default::default()
    };
    assert!(should_include("posts", &config));
    assert!(!should_include("users", &config));
}

// NOTE: resolve_sort and build_find_result tests have been moved to
// db::query::pagination_result::tests (unified PaginationResult builder).
