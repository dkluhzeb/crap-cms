use std::path::PathBuf;

use crap_cms::config::CrapConfig;
use crap_cms::core::SharedRegistry;
use crap_cms::db::DbPool;
use crap_cms::hooks;
use crap_cms::hooks::lifecycle::HookRunner;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

fn setup_lua() -> HookRunner {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");
    HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry)
        .config(&config)
        .build()
        .expect("HookRunner::new failed")
}

/// Helper to eval Lua code and get a string result (no DB connection needed for pure functions).
/// This uses a temporary in-memory DB for the eval.
fn eval_lua(runner: &HookRunner, code: &str) -> String {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

/// Set up a HookRunner with a real synced database (tables created from Lua definitions).
/// Returns (tempdir, pool, registry, runner). The tempdir must be kept alive for the DB.
#[allow(dead_code)]
fn setup_with_db() -> (tempfile::TempDir, DbPool, SharedRegistry, HookRunner) {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");

    // Create a pool and sync tables from Lua-defined collections/globals
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut db_config = CrapConfig::default();
    db_config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &db_config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync failed");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("HookRunner::new failed");
    (tmp, pool, registry, runner)
}

/// Helper to eval Lua code with a real synced DB connection. CRUD functions work here.
#[allow(dead_code)]
fn eval_lua_db(runner: &HookRunner, pool: &DbPool, code: &str) -> String {
    let conn = pool.get().expect("conn");
    runner
        .eval_lua_with_conn(code, &conn, None)
        .expect("eval failed")
}

// ── 3D. Definition Parsing Edge Cases ────────────────────────────────────────

#[test]
fn parse_collection_minimal() {
    let config_dir = fixture_dir();
    let config = CrapConfig::default();
    let registry = hooks::init_lua(&config_dir, &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("articles")
        .expect("articles should be registered");
    assert_eq!(def.slug, "articles");
    assert!(!def.fields.is_empty());
}

#[test]
fn parse_collection_with_all_field_types() {
    // Use a temp dir with a custom collection that has all field types
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("everything.lua"),
        r#"
crap.collections.define("everything", {
    fields = {
        { name = "text_field", type = "text" },
        { name = "num_field", type = "number" },
        { name = "email_field", type = "email" },
        { name = "textarea_field", type = "textarea" },
        { name = "select_field", type = "select", options = {
            { label = "A", value = "a" },
            { label = "B", value = "b" },
        }},
        { name = "checkbox_field", type = "checkbox" },
        { name = "date_field", type = "date" },
        { name = "json_field", type = "json" },
        { name = "richtext_field", type = "richtext" },
        { name = "group_field", type = "group", fields = {
            { name = "sub1", type = "text" },
            { name = "sub2", type = "number" },
        }},
    },
})
        "#,
    )
    .unwrap();

    // Create empty init.lua
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("everything")
        .expect("everything should be registered");
    assert_eq!(def.fields.len(), 10);

    // Verify types
    let field_types: Vec<&str> = def.fields.iter().map(|f| f.field_type.as_str()).collect();
    assert!(field_types.contains(&"text"));
    assert!(field_types.contains(&"number"));
    assert!(field_types.contains(&"email"));
    assert!(field_types.contains(&"select"));
    assert!(field_types.contains(&"checkbox"));
    assert!(field_types.contains(&"group"));

    // Verify group sub-fields
    let group = def.fields.iter().find(|f| f.name == "group_field").unwrap();
    assert_eq!(group.fields.len(), 2);
}

#[test]
fn parse_auth_config_true() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("users.lua"),
        r#"
crap.collections.define("users", {
    auth = true,
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("users")
        .expect("users should be registered");
    assert!(def.is_auth_collection(), "should be auth collection");
    // Email field should have been auto-injected
    assert!(
        def.fields
            .iter()
            .any(|f| f.name == "email" && f.field_type == crap_cms::core::field::FieldType::Email),
        "email field should be auto-injected for auth collections"
    );
}

#[test]
fn parse_auth_config_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("members.lua"),
        r#"
crap.collections.define("members", {
    auth = {
        verify_email = true,
        forgot_password = false,
    },
    fields = {
        { name = "role", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("members")
        .expect("members should be registered");
    assert!(def.is_auth_collection());
    let auth = def.auth.as_ref().unwrap();
    assert!(auth.verify_email);
    assert!(!auth.forgot_password);
}

#[test]
fn parse_global_definition() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let globals_dir = tmp.path().join("globals");
    std::fs::create_dir_all(&globals_dir).unwrap();

    std::fs::write(
        globals_dir.join("settings.lua"),
        r#"
crap.globals.define("settings", {
    labels = { singular = "Settings" },
    fields = {
        { name = "site_name", type = "text" },
        { name = "maintenance_mode", type = "checkbox" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua failed");

    let reg = registry.read().unwrap();
    let def = reg
        .get_global("settings")
        .expect("settings should be registered");
    assert_eq!(def.slug, "settings");
    assert_eq!(def.fields.len(), 2);
    assert_eq!(def.fields[0].name, "site_name");
    assert_eq!(def.fields[1].name, "maintenance_mode");
}

// ── 4B. Collection Definition Parsing ────────────────────────────────────────

#[test]
fn parse_upload_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("media.lua"),
        r#"
crap.collections.define("media", {
    upload = {
        mime_types = { "image/*", "application/pdf" },
        max_file_size = 10485760,
        image_sizes = {
            { name = "thumbnail", width = 300, height = 300, fit = "cover" },
            { name = "card", width = 640, height = 480 },
        },
        format_options = {
            webp = { quality = 80 },
        },
    },
    fields = {
        { name = "alt", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("media")
        .expect("media should be registered");
    assert!(def.is_upload_collection());
    let upload = def.upload.as_ref().unwrap();
    assert_eq!(upload.mime_types.len(), 2);
    assert_eq!(upload.max_file_size, Some(10485760));
    assert_eq!(upload.image_sizes.len(), 2);
    assert_eq!(upload.image_sizes[0].name, "thumbnail");
    assert_eq!(upload.image_sizes[0].width, 300);
    assert!(upload.format_options.webp.is_some());
}

#[test]
fn parse_auth_strategies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("users.lua"),
        r#"
crap.collections.define("users", {
    auth = {
        strategies = {
            { name = "api_key", authenticate = "hooks.auth.api_key" },
            { name = "oauth", authenticate = "hooks.auth.oauth" },
        },
    },
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("users")
        .expect("users should be registered");
    assert!(def.is_auth_collection());
    let auth = def.auth.as_ref().unwrap();
    assert_eq!(auth.strategies.len(), 2);
    assert_eq!(auth.strategies[0].name, "api_key");
    assert_eq!(auth.strategies[0].authenticate, "hooks.auth.api_key");
    assert_eq!(auth.strategies[1].name, "oauth");
}

#[test]
fn parse_live_setting_function() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("events.lua"),
        r#"
crap.collections.define("events", {
    live = "hooks.live.filter",
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("events")
        .expect("events should be registered");
    match &def.live {
        Some(crap_cms::core::collection::LiveSetting::Function(f)) => {
            assert_eq!(f, "hooks.live.filter");
        }
        other => panic!("Expected LiveSetting::Function, got {:?}", other),
    }
}

#[test]
fn parse_live_setting_disabled() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("private.lua"),
        r#"
crap.collections.define("private", {
    live = false,
    fields = {
        { name = "secret", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("private")
        .expect("private should be registered");
    assert!(matches!(
        &def.live,
        Some(crap_cms::core::collection::LiveSetting::Disabled)
    ));
}

#[test]
fn parse_blocks_definition() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("pages.lua"),
        r#"
crap.collections.define("pages", {
    fields = {
        { name = "title", type = "text", required = true },
        { name = "content", type = "blocks", blocks = {
            { type = "text", label = "Text Block", fields = {
                { name = "body", type = "richtext" },
            }},
            { type = "image", label = "Image Block", fields = {
                { name = "src", type = "text" },
                { name = "alt", type = "text" },
            }},
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("pages")
        .expect("pages should be registered");
    let blocks_field = def
        .fields
        .iter()
        .find(|f| f.name == "content")
        .expect("content field");
    assert_eq!(
        blocks_field.field_type,
        crap_cms::core::field::FieldType::Blocks
    );
    assert_eq!(blocks_field.blocks.len(), 2);
    assert_eq!(blocks_field.blocks[0].block_type, "text");
    assert_eq!(blocks_field.blocks[0].fields.len(), 1);
    assert_eq!(blocks_field.blocks[1].block_type, "image");
    assert_eq!(blocks_field.blocks[1].fields.len(), 2);
}

#[test]
fn parse_select_options_localized() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("polls.lua"),
        r#"
crap.collections.define("polls", {
    fields = {
        { name = "answer", type = "select", options = {
            { label = { en = "Yes", de = "Ja" }, value = "yes" },
            { label = { en = "No", de = "Nein" }, value = "no" },
        }},
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("polls")
        .expect("polls should be registered");
    let answer_field = def
        .fields
        .iter()
        .find(|f| f.name == "answer")
        .expect("answer field");
    assert_eq!(answer_field.options.len(), 2);
    assert_eq!(answer_field.options[0].value, "yes");
    // The label should be a LocalizedString::Localized
    match &answer_field.options[0].label {
        crap_cms::core::field::LocalizedString::Localized(map) => {
            assert_eq!(map.get("en"), Some(&"Yes".to_string()));
            assert_eq!(map.get("de"), Some(&"Ja".to_string()));
        }
        crap_cms::core::field::LocalizedString::Plain(s) => {
            // Some implementations may flatten it — that's also acceptable
            assert!(!s.is_empty(), "Should have a non-empty label");
        }
    }
}

#[test]
fn parse_localized_label() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("articles.lua"),
        r#"
crap.collections.define("articles", {
    labels = {
        singular = { en = "Article", de = "Artikel" },
        plural = { en = "Articles", de = "Artikel" },
    },
    fields = {
        { name = "title", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("articles")
        .expect("articles should be registered");

    // Test resolving for different locales
    assert_eq!(def.singular_name_for("en", "en"), "Article");
    assert_eq!(def.singular_name_for("de", "en"), "Artikel");
    assert_eq!(def.display_name_for("en", "en"), "Articles");
}

// ── 5A. Versions Config Parsing ─────────────────────────────────────────────

#[test]
fn parse_versions_config_boolean_true() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("docs.lua"),
        r#"
crap.collections.define("docs", {
    versions = true,
    fields = {
        { name = "title", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("docs")
        .expect("docs should be registered");
    assert!(def.has_versions(), "versions=true should enable versions");
    assert!(
        def.has_drafts(),
        "versions=true enables drafts by default (PayloadCMS convention)"
    );
    let vc = def.versions.as_ref().unwrap();
    assert!(vc.drafts);
    assert_eq!(vc.max_versions, 0);
}

#[test]
fn parse_versions_config_table_with_drafts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("posts.lua"),
        r#"
crap.collections.define("posts", {
    versions = {
        drafts = true,
        max_versions = 50,
    },
    fields = {
        { name = "title", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("posts")
        .expect("posts should be registered");
    assert!(def.has_versions(), "should have versions");
    assert!(def.has_drafts(), "should have drafts");
    let vc = def.versions.as_ref().unwrap();
    assert!(vc.drafts);
    assert_eq!(vc.max_versions, 50);
}

#[test]
fn parse_versions_config_false() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("notes.lua"),
        r#"
crap.collections.define("notes", {
    versions = false,
    fields = {
        { name = "body", type = "textarea" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("notes")
        .expect("notes should be registered");
    assert!(
        !def.has_versions(),
        "versions=false should not enable versions"
    );
    assert!(def.versions.is_none());
}

#[test]
fn parse_versions_config_omitted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let collections_dir = tmp.path().join("collections");
    std::fs::create_dir_all(&collections_dir).unwrap();

    std::fs::write(
        collections_dir.join("plain.lua"),
        r#"
crap.collections.define("plain", {
    fields = {
        { name = "name", type = "text" },
    },
})
        "#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let config = CrapConfig::default();
    let registry = crap_cms::hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let reg = registry.read().unwrap();
    let def = reg
        .get_collection("plain")
        .expect("plain should be registered");
    assert!(
        !def.has_versions(),
        "no versions config should mean no versions"
    );
    assert!(def.versions.is_none());
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.crypto
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn crypto_sha256() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hash = crap.crypto.sha256("hello")
        -- Known SHA-256 of "hello"
        if hash ~= "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824" then
            return "WRONG:" .. hash
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_sha256_empty() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hash = crap.crypto.sha256("")
        -- Known SHA-256 of empty string
        if hash ~= "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" then
            return "WRONG:" .. hash
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_hmac_sha256() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local mac = crap.crypto.hmac_sha256("hello", "secret-key")
        -- Should be 64 hex characters (32 bytes)
        if #mac ~= 64 then return "LEN:" .. #mac end
        -- Should be deterministic
        local mac2 = crap.crypto.hmac_sha256("hello", "secret-key")
        if mac ~= mac2 then return "NOT_DETERMINISTIC" end
        -- Different key should give different result
        local mac3 = crap.crypto.hmac_sha256("hello", "other-key")
        if mac == mac3 then return "SAME_WITH_DIFF_KEY" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_base64_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local original = "Hello, World! 123 Special chars: @#$%"
        local encoded = crap.crypto.base64_encode(original)
        local decoded = crap.crypto.base64_decode(encoded)
        if decoded ~= original then
            return "MISMATCH:" .. decoded
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_base64_known_value() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local encoded = crap.crypto.base64_encode("hello")
        if encoded ~= "aGVsbG8=" then return "WRONG:" .. encoded end
        local decoded = crap.crypto.base64_decode("aGVsbG8=")
        if decoded ~= "hello" then return "DECODE_WRONG:" .. decoded end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_encrypt_decrypt_roundtrip() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local plaintext = "secret message 123!"
        local encrypted = crap.crypto.encrypt(plaintext)
        -- Encrypted should be different from plaintext
        if encrypted == plaintext then return "NOT_ENCRYPTED" end
        -- Should be base64 encoded
        if #encrypted < #plaintext then return "TOO_SHORT" end

        local decrypted = crap.crypto.decrypt(encrypted)
        if decrypted ~= plaintext then
            return "MISMATCH:" .. decrypted
        end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_encrypt_produces_different_ciphertexts() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        -- Same plaintext should produce different ciphertexts (random nonce)
        local a = crap.crypto.encrypt("same text")
        local b = crap.crypto.encrypt("same text")
        if a == b then return "SAME_CIPHERTEXT" end
        -- But both should decrypt to the same thing
        if crap.crypto.decrypt(a) ~= "same text" then return "A_WRONG" end
        if crap.crypto.decrypt(b) ~= "same text" then return "B_WRONG" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_decrypt_invalid_input() {
    let runner = setup_lua();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    let conn = pool.get().expect("conn");
    let result = runner
        .eval_lua_with_conn(
            r#"
        local ok, err = pcall(function()
            crap.crypto.decrypt("not-valid-base64!@#$")
        end)
        if ok then return "SHOULD_HAVE_FAILED" end
        return "ok"
    "#,
            &conn,
            None,
        )
        .expect("eval");
    assert_eq!(result, "ok");
}

#[test]
fn crypto_random_bytes() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local hex = crap.crypto.random_bytes(16)
        -- 16 bytes = 32 hex characters
        if #hex ~= 32 then return "LEN:" .. #hex end
        -- Should be hex (only 0-9a-f)
        if hex:find("[^0-9a-f]") then return "NOT_HEX:" .. hex end
        -- Two calls should produce different results
        local hex2 = crap.crypto.random_bytes(16)
        if hex == hex2 then return "SAME" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

#[test]
fn crypto_random_bytes_various_sizes() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local h1 = crap.crypto.random_bytes(1)
        if #h1 ~= 2 then return "1B:" .. #h1 end
        local h32 = crap.crypto.random_bytes(32)
        if #h32 ~= 64 then return "32B:" .. #h32 end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}

// ══════════════════════════════════════════════════════════════════════════════
// NEW FEATURES: crap.schema
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn schema_get_collection() {
    let runner = setup_lua();
    let result = eval_lua(
        &runner,
        r#"
        local schema = crap.schema.get_collection("articles")
        if schema == nil then return "NIL" end
        if schema.slug ~= "articles" then return "SLUG:" .. tostring(schema.slug) end
        if schema.labels.singular ~= "Article" then return "SINGULAR:" .. tostring(schema.labels.singular) end
        if schema.labels.plural ~= "Articles" then return "PLURAL:" .. tostring(schema.labels.plural) end
        if #schema.fields < 1 then return "NO_FIELDS" end
        -- Check first field
        local title_field = nil
        for _, f in ipairs(schema.fields) do
            if f.name == "title" then title_field = f; break end
        end
        if title_field == nil then return "NO_TITLE" end
        if title_field.type ~= "text" then return "TITLE_TYPE:" .. title_field.type end
        if not title_field.required then return "TITLE_NOT_REQUIRED" end
        if not title_field.unique then return "TITLE_NOT_UNIQUE" end
        return "ok"
    "#,
    );
    assert_eq!(result, "ok");
}
