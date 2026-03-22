use std::collections::HashMap;
use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::Document;
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
    crap_cms::core::SharedRegistry,
    HookRunner,
) {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
    migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
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
    registry: &crap_cms::core::SharedRegistry,
) -> Vec<String> {
    let reg = registry.read().unwrap();
    let def = reg.get_collection("items").unwrap().clone();
    drop(reg);

    let rows = vec![
        ("Item A", "editor-1", "draft", "secret-a"),
        ("Item B", "editor-1", "published", "secret-b"),
        ("Item C", "other-1", "draft", "secret-c"),
    ];

    let mut ids = Vec::new();
    for (title, owner, status, notes) in rows {
        let mut data = HashMap::new();
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
fn find_override_access_true_returns_all() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items")
        return tostring(r.pagination.totalDocs)
        "#,
            &conn,
            None, // no user — doesn't matter when overrideAccess=true (default)
        )
        .unwrap();

    assert_eq!(result, "3", "overrideAccess=true should return all items");
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
        local r = crap.collections.find("items", { overrideAccess = false })
        return tostring(r.pagination.totalDocs)
        "#,
            &conn,
            Some(&admin),
        )
        .unwrap();

    assert_eq!(
        result, "3",
        "admin with overrideAccess=false should see all items"
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
        local r = crap.collections.find("items", { overrideAccess = false })
        return tostring(r.pagination.totalDocs)
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
        local r = crap.collections.find("items", { overrideAccess = false })
        return tostring(r.pagination.totalDocs)
        "#,
        &conn,
        None, // anonymous
    );

    assert!(
        result.is_err(),
        "anonymous find with overrideAccess=false should error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Read access denied"),
        "error should mention access denied, got: {}",
        err
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
        local r = crap.collections.find("items", { overrideAccess = false })
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
        local r = crap.collections.find("items", { overrideAccess = false })
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
        local doc = crap.collections.find_by_id("items", "{}", {{ overrideAccess = false }})
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
        local doc = crap.collections.find_by_id("items", "{}", {{ overrideAccess = false }})
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
        local doc = crap.collections.find_by_id("items", "{}", {{ overrideAccess = false }})
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
        local doc = crap.collections.find_by_id("items", "{}", {{ overrideAccess = false }})
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
        local doc = crap.collections.find_by_id("items", "{}", {{ overrideAccess = false }})
        return "FOUND"
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, None);
    assert!(result.is_err(), "anonymous find_by_id should be denied");
}

// ── create ──────────────────────────────────────────────────────────────────

#[test]
fn create_override_access_true_works_without_user() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().unwrap();
    let result = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("items", { title = "Test" })
        return doc.id
        "#,
            &conn,
            None,
        )
        .unwrap();

    assert!(
        !result.is_empty(),
        "create with default overrideAccess should work"
    );
}

#[test]
fn create_override_access_false_anonymous_denied() {
    let (_tmp, pool, _registry, runner) = setup();

    let conn = pool.get().unwrap();
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("items", { title = "Test" }, { overrideAccess = false })
        return doc.id
        "#,
        &conn,
        None,
    );

    assert!(
        result.is_err(),
        "anonymous create with overrideAccess=false should error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Create access denied"),
        "error should mention access denied, got: {}",
        err
    );
}

#[test]
fn create_override_access_false_editor_allowed() {
    let (_tmp, pool, _registry, runner) = setup();
    let editor = make_user("editor-1", "editor");

    let conn = pool.get().unwrap();
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("items", { title = "Editor Post" }, { overrideAccess = false })
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
        }, { overrideAccess = false })
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
        }, { overrideAccess = false })
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
        local doc = crap.collections.update("items", "{}", {{ title = "New" }}, {{ overrideAccess = false }})
        return doc.title
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, None);
    assert!(
        result.is_err(),
        "anonymous update with overrideAccess=false should error"
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
        local doc = crap.collections.update("items", "{}", {{ title = "Updated" }}, {{ overrideAccess = false }})
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
        }}, {{ overrideAccess = false }})
        return doc.status .. "|" .. (doc.notes or "NIL")
        "#,
        ids[0] // Item A, status=draft, notes=secret-a
    );
    let result = runner
        .eval_lua_with_conn(&code, &conn, Some(&editor))
        .unwrap();
    // status should remain "draft" (update was stripped), notes should remain "secret-a"
    assert_eq!(
        result, "draft|secret-a",
        "editor's status and notes updates should be stripped"
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
        }}, {{ overrideAccess = false }})
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
fn delete_override_access_true_works_without_user() {
    let (_tmp, pool, registry, runner) = setup();
    let ids = seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    let code = format!(
        r#"
        crap.collections.delete("items", "{}")
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
        crap.collections.delete("items", "{}", {{ overrideAccess = false }})
        return "OK"
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, None);
    assert!(
        result.is_err(),
        "anonymous delete with overrideAccess=false should error"
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
        crap.collections.delete("items", "{}", {{ overrideAccess = false }})
        return "OK"
        "#,
        ids[0]
    );
    let result = runner.eval_lua_with_conn(&code, &conn, Some(&editor));
    assert!(
        result.is_err(),
        "editor delete with overrideAccess=false should error (admin_only)"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("access denied") || err.contains("Delete access denied"),
        "error should mention access denied, got: {}",
        err
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
        crap.collections.delete("items", "{}", {{ overrideAccess = false }})
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
    // Create with overrideAccess=true works without user
    let result = runner
        .eval_lua_with_conn(
            r#"
        local doc = crap.collections.create("items", { title = "No User" })
        return doc.id
        "#,
            &conn,
            None,
        )
        .unwrap();
    assert!(!result.is_empty());

    // But overrideAccess=false without user is denied (authenticated required)
    let result = runner.eval_lua_with_conn(
        r#"
        local doc = crap.collections.create("items", { title = "No User" }, { overrideAccess = false })
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
        local r = crap.collections.find("items", { overrideAccess = false })
        return tostring(r.pagination.totalDocs)
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
fn default_override_access_is_true() {
    let (_tmp, pool, registry, runner) = setup();
    seed_items(&pool, &registry);

    let conn = pool.get().unwrap();
    // Without specifying overrideAccess at all, should behave as true
    let result = runner
        .eval_lua_with_conn(
            r#"
        local r = crap.collections.find("items", {})
        return tostring(r.pagination.totalDocs)
        "#,
            &conn,
            None, // no user, but default overrideAccess=true means no check
        )
        .unwrap();

    assert_eq!(
        result, "3",
        "default (no overrideAccess specified) should bypass access control"
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
        local r = crap.collections.find("items", { overrideAccess = true })
        return tostring(r.pagination.totalDocs)
        "#,
            &conn,
            None,
        )
        .unwrap();

    assert_eq!(
        result, "3",
        "explicit overrideAccess=true should bypass access control"
    );
}
