#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::path::PathBuf;
use std::sync::Arc;

use crap_cms::config::CrapConfig;
use crap_cms::core::Document;
use crap_cms::core::DocumentFields;
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;
use serde_json::json;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/override_access")
}

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    std::sync::Arc<crap_cms::core::Registry>,
    HookRunner,
) {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();
    (tmp, db_pool, registry, runner)
}

fn make_user(id: &str, role: &str) -> Document {
    let mut doc = Document::new(id.to_string());
    doc.fields.insert("role".into(), json!(role));
    doc.fields
        .insert("email".into(), json!(format!("{}@test.com", id)));
    doc
}

/// Seed items: two owned by "editor-1", one by "other-1", all with notes.
fn seed_items(
    pool: &crap_cms::db::DbPool,
    registry: &std::sync::Arc<crap_cms::core::Registry>,
) -> Vec<String> {
    let def = registry.get_collection("items").unwrap().clone();

    let rows = vec![
        ("Item A", "editor-1", "draft", "secret-a"),
        ("Item B", "editor-1", "published", "secret-b"),
        ("Item C", "other-1", "draft", "secret-c"),
    ];

    let mut ids = Vec::new();
    for (title, owner, status, notes) in rows {
        let mut data = DocumentFields::new();
        data.insert("title".into(), title.into());
        data.insert("owner".into(), owner.into());
        data.insert("status".into(), status.into());
        data.insert("notes".into(), notes.into());

        let mut conn = pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "items", &def, &data, None).unwrap();
        ids.push(doc.id.to_string());
        tx.commit().unwrap();
    }
    ids
}

// ── find ────────────────────────────────────────────────────────────────────

#[test]
fn find_explicit_override_access_true_returns_all() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items", { override_access = true })
        return tostring(r.pagination.total_docs)
        "#,
            &conn,
            None, // no user — doesn't matter when override_access=true
        )
        .unwrap();

    assert_eq!(result, "3", "override_access=true should return all items");
}

#[test]
fn find_override_access_false_admin_returns_all() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);
    let admin = make_user("admin-1", "admin");

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items", { override_access = false })
        return tostring(r.pagination.total_docs)
        "#,
            &conn,
            Some(&admin),
        )
        .unwrap();

    assert_eq!(
        result, "3",
        "admin with override_access=false should see all items"
    );
}

#[test]
fn find_override_access_false_editor_sees_only_own() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items", { override_access = false })
        return tostring(r.pagination.total_docs)
        "#,
            &conn,
            Some(&editor),
        )
        .unwrap();

    assert_eq!(
        result, "2",
        "editor should only see own items (owner=editor-1)"
    );
}

#[test]
fn find_override_access_false_anonymous_denied() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let result = runner.eval_lua_with_conn(
        r#"
        local r = crap.collections.find("items", { override_access = false })
        return tostring(r.pagination.total_docs)
        "#,
        &conn,
        None, // anonymous
    );

    assert!(
        result.is_err(),
        "anonymous find with override_access=false should error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Read access denied"),
        "error should mention access denied, got: {err}"
    );
}

#[test]
fn find_override_access_false_strips_denied_read_fields() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    // notes field has read access = admin_only, so editor should not see it
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items", { override_access = false })
        local has_notes = false
        for _, doc in ipairs(r.documents) do
            if doc.notes ~= nil then has_notes = true end
        end
        return tostring(has_notes)
        "#,
            &conn,
            Some(&editor),
        )
        .unwrap();

    assert_eq!(
        result, "false",
        "editor should not see 'notes' field (admin-only read)"
    );
}

#[test]
fn find_override_access_false_admin_sees_all_fields() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);
    let admin = make_user("admin-1", "admin");

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items", { override_access = false })
        local notes_count = 0
        for _, doc in ipairs(r.documents) do
            if doc.notes ~= nil then notes_count = notes_count + 1 end
        end
        return tostring(notes_count)
        "#,
            &conn,
            Some(&admin),
        )
        .unwrap();

    assert_eq!(result, "3", "admin should see 'notes' field on all items");
}

// ── find_by_id ──────────────────────────────────────────────────────────────

#[test]
fn find_by_id_override_access_false_admin_returns_doc() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let admin = make_user("admin-1", "admin");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.find_by_id("items", "{}", {{ override_access = false }})
        if doc then return doc.title else return "NIL" end
        "#,
        ids[0]
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&admin))
        .unwrap();
    assert_eq!(result, "Item A");
}

#[test]
fn find_by_id_override_access_false_editor_own_item_ok() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    // ids[0] is owned by editor-1 → should be accessible
    let code = format!(
        r#"
        local doc = crap.collections.find_by_id("items", "{}", {{ override_access = false }})
        if doc then return doc.title else return "NIL" end
        "#,
        ids[0]
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&editor))
        .unwrap();
    assert_eq!(result, "Item A");
}

#[test]
fn find_by_id_override_access_false_editor_other_item_nil() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    // ids[2] is owned by other-1 → constrained read should return nil
    let code = format!(
        r#"
        local doc = crap.collections.find_by_id("items", "{}", {{ override_access = false }})
        if doc then return "FOUND" else return "NIL" end
        "#,
        ids[2]
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&editor))
        .unwrap();
    assert_eq!(result, "NIL", "editor should not see other user's item");
}

#[test]
fn find_by_id_override_access_false_strips_read_fields() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.find_by_id("items", "{}", {{ override_access = false }})
        if doc and doc.notes ~= nil then return "HAS_NOTES" else return "NO_NOTES" end
        "#,
        ids[0]
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&editor))
        .unwrap();
    assert_eq!(
        result, "NO_NOTES",
        "editor should not see 'notes' on find_by_id"
    );
}

#[test]
fn find_by_id_override_access_false_anonymous_denied() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.find_by_id("items", "{}", {{ override_access = false }})
        return "FOUND"
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, None);
    assert!(result.is_err(), "anonymous find_by_id should be denied");
}

// ── create ──────────────────────────────────────────────────────────────────

#[test]
fn create_explicit_override_access_true_works_without_user() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("items", { title = "Test" }, { override_access = true })
        return doc.id
        "#,
            &conn,
            None,
        )
        .unwrap();

    assert!(
        !result.is_empty(),
        "create with explicit override_access=true should work"
    );
}

#[test]
fn create_override_access_false_anonymous_denied() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().unwrap();
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("items", { title = "Test" }, { override_access = false })
        return doc.id
        "#,
        &conn,
        None,
    );

    assert!(
        result.is_err(),
        "anonymous create with override_access=false should error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Create access denied"),
        "error should mention access denied, got: {err}"
    );
}

#[test]
fn create_override_access_false_editor_allowed() {
    let (_tmp, pool, _registry, runner) = setup();
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("items", { title = "Editor Post" }, { override_access = false })
        return doc.title
        "#,
        &conn,
        Some(&editor),
    ).unwrap();

    assert_eq!(result, "Editor Post");
}

#[test]
fn create_override_access_false_strips_denied_write_fields() {
    let (_tmp, pool, _registry, runner) = setup();
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    // 'notes' has create access = admin_only → should be stripped for editor
    let result = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("items", {
            title = "Test",
            notes = "should-be-stripped",
        }, { override_access = false })
        if doc.notes == nil or doc.notes == "" then return "STRIPPED" else return doc.notes end
        "#,
            &conn,
            Some(&editor),
        )
        .unwrap();

    assert_eq!(
        result, "STRIPPED",
        "editor's 'notes' should be stripped on create"
    );
}

#[test]
fn create_override_access_false_admin_keeps_all_fields() {
    let (_tmp, pool, _registry, runner) = setup();
    let admin = make_user("admin-1", "admin");

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("items", {
            title = "Admin Post",
            notes = "admin-notes",
        }, { override_access = false })
        return doc.notes or "MISSING"
        "#,
            &conn,
            Some(&admin),
        )
        .unwrap();

    assert_eq!(result, "admin-notes", "admin should keep 'notes' on create");
}

// ── update ──────────────────────────────────────────────────────────────────

#[test]
fn update_override_access_false_anonymous_denied() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.update("items", "{}", {{ title = "New" }}, {{ override_access = false }})
        return doc.title
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, None);
    assert!(
        result.is_err(),
        "anonymous update with override_access=false should error"
    );
}

#[test]
fn update_override_access_false_editor_allowed() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.update("items", "{}", {{ title = "Updated" }}, {{ override_access = false }})
        return doc.title
        "#,
        ids[0]
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&editor))
        .unwrap();
    assert_eq!(result, "Updated");
}

#[test]
fn update_override_access_false_strips_status_for_editor() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    // status has update access = admin_only → stripped for editor
    // notes has update access = admin_only → stripped for editor
    let code = format!(
        r#"
        local doc = crap.collections.update("items", "{}", {{
            title = "Updated",
            status = "published",
            notes = "new-notes",
        }}, {{ override_access = false }})
        return doc.status .. "|" .. (doc.notes or "NIL")
        "#,
        ids[0] // Item A, status=draft, notes=secret-a
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&editor))
        .unwrap();
    // status should remain "draft" (update was stripped),
    // notes is read-denied for editor so it comes back as NIL
    assert_eq!(
        result, "draft|NIL",
        "editor's status and notes updates should be stripped, notes read-denied"
    );
}

#[test]
fn update_override_access_false_admin_updates_all_fields() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let admin = make_user("admin-1", "admin");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        local doc = crap.collections.update("items", "{}", {{
            status = "published",
            notes = "admin-updated",
        }}, {{ override_access = false }})
        return doc.status .. "|" .. doc.notes
        "#,
        ids[0]
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&admin))
        .unwrap();
    assert_eq!(
        result, "published|admin-updated",
        "admin should update all fields including status and notes"
    );
}

// ── delete ──────────────────────────────────────────────────────────────────

#[test]
fn delete_explicit_override_access_true_works_without_user() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("items", "{}", {{ override_access = true }})
        return "OK"
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, None).unwrap();
    assert_eq!(result, "OK");
}

#[test]
fn delete_override_access_false_anonymous_denied() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("items", "{}", {{ override_access = false }})
        return "OK"
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, None);
    assert!(
        result.is_err(),
        "anonymous delete with override_access=false should error"
    );
}

#[test]
fn delete_override_access_false_editor_denied() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("items", "{}", {{ override_access = false }})
        return "OK"
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, Some(&editor));
    assert!(
        result.is_err(),
        "editor delete with override_access=false should error (admin_only)"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Delete access denied"),
        "error should mention access denied, got: {err}"
    );
}

#[test]
fn delete_override_access_false_admin_allowed() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);
    let admin = make_user("admin-1", "admin");

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("items", "{}", {{ override_access = false }})
        return "OK"
        "#,
        ids[0]
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&admin))
        .unwrap();
    assert_eq!(result, "OK");

    // Verify it's actually deleted
    let code2 = format!(
        r#"
        local doc = crap.collections.find_by_id("items", "{}")
        if doc then return "EXISTS" else return "DELETED" end
        "#,
        ids[0]
    );
    let result2 = runner
        .eval_lua_with_conn(&code2, &conn, Some(&admin))
        .unwrap();
    assert_eq!(
        result2, "DELETED",
        "item should be actually deleted from DB"
    );
}

// ── user context propagation ────────────────────────────────────────────────

#[test]
fn user_context_none_when_no_user_provided() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().unwrap();
    // Create with explicit override_access=true works without user
    let result = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("items", { title = "No User" }, { override_access = true })
        return doc.id
        "#,
            &conn,
            None,
        )
        .unwrap();
    assert!(!result.is_empty());

    // Default override_access=false without user is denied (authenticated required)
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("items", { title = "No User" })
        return doc.id
        "#,
        &conn,
        None,
    );
    assert!(result.is_err());
}

#[test]
fn user_context_propagated_correctly() {
    let (_tmp, pool, _registry, runner) = setup();
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    // Create an item, then find it with constrained access
    let result = runner
        .eval_lua_with_conn(
            r#"
        crap.collections.create("items", { title = "Mine", owner = "editor-1" })
        crap.collections.create("items", { title = "Theirs", owner = "other-1" })
        local r = crap.collections.find("items", { override_access = false })
        return tostring(r.pagination.total_docs)
        "#,
            &conn,
            Some(&editor),
        )
        .unwrap();

    assert_eq!(
        result, "1",
        "editor should only find their own items via constrained access"
    );
}

// ── default behavior: backward compatible ───────────────────────────────────

#[test]
fn default_override_access_is_false() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    // Without specifying override_access at all, should enforce access control.
    // With no user provided, anonymous access is denied.
    let result = runner.eval_lua_with_conn(
        r#"
        local r = crap.collections.find("items", {})
        return tostring(r.pagination.total_docs)
        "#,
        &conn,
        None, // no user — default override_access=false means access check runs
    );

    assert!(
        result.is_err(),
        "default (no override_access specified) should enforce access control"
    );
}

#[test]
fn explicit_override_access_true_bypasses_all() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items", { override_access = true })
        return tostring(r.pagination.total_docs)
        "#,
            &conn,
            None,
        )
        .unwrap();

    assert_eq!(
        result, "3",
        "explicit override_access=true should bypass access control"
    );
}
