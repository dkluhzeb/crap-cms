use std::collections::{HashMap, HashSet};

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Registry;
use crap_cms::core::collection::{CollectionDefinition, GlobalDefinition};
use crap_cms::core::field::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};
use crap_cms::db::{DbConnection, migrate, ops, pool, query};
use serde_json::json;

fn create_test_pool() -> (tempfile::TempDir, crap_cms::db::DbPool) {
    let tmp = tempfile::tempdir().expect("Failed to create temp dir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &config).expect("Failed to create pool");
    (tmp, db_pool)
}

fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

// ── Locale-Aware Query Tests ──────────────────────────────────────────────────

fn make_localized_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("localized_pages");
    def.timestamps = true;
    let title = FieldDefinition {
        name: "title".to_string(),
        localized: true,
        ..Default::default()
    };
    def.fields = vec![title, make_field("slug_field", FieldType::Text)];
    def
}

fn setup_localized() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
    LocaleConfig,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    migrate::sync_all(&pool, &registry, &locale_config).expect("Sync");
    (_tmp, pool, def, locale_config)
}

#[test]
fn create_with_locale_writes_correct_column() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    let locale_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "English Title".to_string());
    data.insert("slug_field".to_string(), "test-page".to_string());

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc =
        query::create(&tx, "localized_pages", &def, &data, Some(&locale_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // In Single mode, the title is returned as "title" (aliased)
    assert_eq!(doc.get_str("title"), Some("English Title"));
}

#[test]
fn find_with_locale_coalesce_fallback() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    // Create with English locale
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "English Title".to_string());
    data.insert("slug_field".to_string(), "test-page".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // Read with German locale — should fall back to English
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let docs = ops::find_documents(
        &pool,
        "localized_pages",
        &def,
        &query::FindQuery::default(),
        Some(&de_ctx),
    )
    .expect("Find");
    assert!(!docs.is_empty());
    // With fallback enabled, should get the English value
    assert_eq!(docs[0].get_str("title"), Some("English Title"));
}

#[test]
fn find_all_locales_returns_nested() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    // Create with English
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "English Title".to_string());
    data.insert("slug_field".to_string(), "page".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // Update with German
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let mut de_data = HashMap::new();
    de_data.insert("title".to_string(), "Deutscher Titel".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    query::update(
        &tx,
        "localized_pages",
        &def,
        &doc.id,
        &de_data,
        Some(&de_ctx),
    )
    .expect("Update");
    tx.commit().expect("Commit");

    // Find with All mode — should return nested locale object
    let all_ctx = query::LocaleContext {
        mode: query::LocaleMode::All,
        config: locale_config.clone(),
    };
    let docs = ops::find_documents(
        &pool,
        "localized_pages",
        &def,
        &query::FindQuery::default(),
        Some(&all_ctx),
    )
    .expect("Find");
    assert!(!docs.is_empty());

    let title_val = docs[0].get("title").expect("title should exist");
    assert!(
        title_val.is_object(),
        "title should be a locale object, got: {:?}",
        title_val
    );
    assert_eq!(
        title_val.get("en").unwrap().as_str().unwrap(),
        "English Title"
    );
    assert_eq!(
        title_val.get("de").unwrap().as_str().unwrap(),
        "Deutscher Titel"
    );
}

#[test]
fn update_with_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Original".to_string());
    data.insert("slug_field".to_string(), "test".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
    tx.commit().expect("Commit");

    // Update the English title
    let mut update = HashMap::new();
    update.insert("title".to_string(), "Updated English".to_string());
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let updated = query::update(
        &tx,
        "localized_pages",
        &def,
        &doc.id,
        &update,
        Some(&en_ctx),
    )
    .expect("Update");
    tx.commit().expect("Commit");
    assert_eq!(updated.get_str("title"), Some("Updated English"));
}

#[test]
fn filter_on_localized_field() {
    let (_tmp, pool, def, locale_config) = setup_localized();

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    // Create two documents
    for (title, slug) in &[("Hello World", "hello"), ("Goodbye World", "goodbye")] {
        let mut data = HashMap::new();
        data.insert("title".to_string(), title.to_string());
        data.insert("slug_field".to_string(), slug.to_string());
        let mut conn = pool.get().expect("conn");
        let tx = conn.transaction().expect("tx");
        query::create(&tx, "localized_pages", &def, &data, Some(&en_ctx)).expect("Create");
        tx.commit().expect("Commit");
    }

    // Filter on localized field
    let q = query::FindQuery::builder()
        .filters(vec![query::FilterClause::Single(query::Filter {
            field: "title".to_string(),
            op: query::FilterOp::Contains("Hello".to_string()),
        })])
        .build();
    let docs =
        ops::find_documents(&pool, "localized_pages", &def, &q, Some(&en_ctx)).expect("Find");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Hello World"));
}

// ── Locale-Aware Join Table Tests ────────────────────────────────────────────

/// Collection definition with localized join-table fields (has-many, array, blocks).
fn make_localized_join_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("l10n_articles");
    def.timestamps = true;
    let tags_field = FieldDefinition {
        name: "tags".to_string(),
        field_type: FieldType::Relationship,
        localized: true,
        relationship: Some(RelationshipConfig::new("tags", true)),
        ..Default::default()
    };
    let links_field = FieldDefinition {
        name: "links".to_string(),
        field_type: FieldType::Array,
        localized: true,
        fields: vec![
            make_field("url", FieldType::Text),
            make_field("label", FieldType::Text),
        ],
        ..Default::default()
    };
    let content_field = FieldDefinition {
        name: "content".to_string(),
        field_type: FieldType::Blocks,
        localized: true,
        blocks: vec![BlockDefinition::new(
            "paragraph",
            vec![make_field("text", FieldType::Textarea)],
        )],
        ..Default::default()
    };
    let meta_field = FieldDefinition {
        name: "meta".to_string(),
        field_type: FieldType::Blocks,
        blocks: vec![BlockDefinition::new(
            "kv",
            vec![make_field("key", FieldType::Text)],
        )],
        ..Default::default()
    };
    def.fields = vec![
        make_field("slug_field", FieldType::Text),
        tags_field,
        links_field,
        content_field,
        meta_field,
    ];
    def
}

fn setup_localized_joins() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
    LocaleConfig,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_join_def();
    let mut tags_def = CollectionDefinition::new("tags");
    tags_def.timestamps = true;
    tags_def.fields = vec![make_field("name", FieldType::Text)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_collection(tags_def);
    }
    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    migrate::sync_all(&pool, &registry, &locale_config).expect("Sync");
    (_tmp, pool, def, locale_config)
}

// ── Low-level: set/find with locale scoping ──────────────────────────────────

#[test]
fn localized_related_ids_scoped_by_locale() {
    let (_tmp, pool, _def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    // Create a parent document
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &_def, &data, None).expect("Create");

    // Write English tags
    query::set_related_ids(
        &tx,
        "l10n_articles",
        "tags",
        &doc.id,
        &["en-tag-1".into(), "en-tag-2".into()],
        Some("en"),
    )
    .expect("Set EN tags");
    // Write German tags
    query::set_related_ids(
        &tx,
        "l10n_articles",
        "tags",
        &doc.id,
        &["de-tag-1".into()],
        Some("de"),
    )
    .expect("Set DE tags");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    // Read English tags — should only get EN
    let en_tags = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("en"))
        .expect("Find EN tags");
    assert_eq!(en_tags, vec!["en-tag-1", "en-tag-2"]);

    // Read German tags — should only get DE
    let de_tags = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("de"))
        .expect("Find DE tags");
    assert_eq!(de_tags, vec!["de-tag-1"]);
}

#[test]
fn localized_related_ids_update_one_locale_preserves_other() {
    let (_tmp, pool, _def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &_def, &data, None).expect("Create");

    // Write both locales
    query::set_related_ids(
        &tx,
        "l10n_articles",
        "tags",
        &doc.id,
        &["en-1".into(), "en-2".into()],
        Some("en"),
    )
    .expect("Set EN");
    query::set_related_ids(
        &tx,
        "l10n_articles",
        "tags",
        &doc.id,
        &["de-1".into()],
        Some("de"),
    )
    .expect("Set DE");

    // Overwrite EN tags — DE should be preserved
    query::set_related_ids(
        &tx,
        "l10n_articles",
        "tags",
        &doc.id,
        &["en-3".into()],
        Some("en"),
    )
    .expect("Overwrite EN");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("en")).unwrap();
    assert_eq!(en, vec!["en-3"], "EN tags should be replaced");

    let de = query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("de")).unwrap();
    assert_eq!(de, vec!["de-1"], "DE tags should be preserved");
}

#[test]
fn localized_array_rows_scoped_by_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    let en_rows = vec![HashMap::from([
        ("url".to_string(), "https://en.example.com".to_string()),
        ("label".to_string(), "English Link".to_string()),
    ])];
    let de_rows = vec![HashMap::from([
        ("url".to_string(), "https://de.example.com".to_string()),
        ("label".to_string(), "German Link".to_string()),
    ])];

    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc.id,
        &en_rows,
        &links_field.fields,
        Some("en"),
    )
    .expect("Set EN links");
    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc.id,
        &de_rows,
        &links_field.fields,
        Some("de"),
    )
    .expect("Set DE links");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_array_rows(
        &conn,
        "l10n_articles",
        "links",
        &doc.id,
        &links_field.fields,
        Some("en"),
    )
    .unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["label"], "English Link");

    let de = query::find_array_rows(
        &conn,
        "l10n_articles",
        "links",
        &doc.id,
        &links_field.fields,
        Some("de"),
    )
    .unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(de[0]["label"], "German Link");
}

#[test]
fn localized_array_rows_update_preserves_other_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    let en_rows = vec![HashMap::from([
        ("url".to_string(), "https://en.example.com".to_string()),
        ("label".to_string(), "English".to_string()),
    ])];
    let de_rows = vec![HashMap::from([
        ("url".to_string(), "https://de.example.com".to_string()),
        ("label".to_string(), "Deutsch".to_string()),
    ])];

    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc.id,
        &en_rows,
        &links_field.fields,
        Some("en"),
    )
    .unwrap();
    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc.id,
        &de_rows,
        &links_field.fields,
        Some("de"),
    )
    .unwrap();

    // Replace EN rows — DE should remain
    let en_new = vec![HashMap::from([
        ("url".to_string(), "https://new-en.example.com".to_string()),
        ("label".to_string(), "New English".to_string()),
    ])];
    query::set_array_rows(
        &tx,
        "l10n_articles",
        "links",
        &doc.id,
        &en_new,
        &links_field.fields,
        Some("en"),
    )
    .unwrap();
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en = query::find_array_rows(
        &conn,
        "l10n_articles",
        "links",
        &doc.id,
        &links_field.fields,
        Some("en"),
    )
    .unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["label"], "New English");

    let de = query::find_array_rows(
        &conn,
        "l10n_articles",
        "links",
        &doc.id,
        &links_field.fields,
        Some("de"),
    )
    .unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(
        de[0]["label"], "Deutsch",
        "DE array rows should be preserved"
    );
}

#[test]
fn localized_block_rows_scoped_by_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    let en_blocks = vec![json!({"_block_type": "paragraph", "text": "Hello world"})];
    let de_blocks = vec![json!({"_block_type": "paragraph", "text": "Hallo Welt"})];

    query::set_block_rows(
        &tx,
        "l10n_articles",
        "content",
        &doc.id,
        &en_blocks,
        Some("en"),
    )
    .unwrap();
    query::set_block_rows(
        &tx,
        "l10n_articles",
        "content",
        &doc.id,
        &de_blocks,
        Some("de"),
    )
    .unwrap();
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en =
        query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("en")).unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["text"], "Hello world");

    let de =
        query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("de")).unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(de[0]["text"], "Hallo Welt");
}

#[test]
fn localized_block_rows_update_preserves_other_locale() {
    let (_tmp, pool, def, _lc) = setup_localized_joins();
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    query::set_block_rows(
        &tx,
        "l10n_articles",
        "content",
        &doc.id,
        &[json!({"_block_type": "paragraph", "text": "English"})],
        Some("en"),
    )
    .unwrap();
    query::set_block_rows(
        &tx,
        "l10n_articles",
        "content",
        &doc.id,
        &[json!({"_block_type": "paragraph", "text": "Deutsch"})],
        Some("de"),
    )
    .unwrap();

    // Replace EN blocks — DE should remain
    query::set_block_rows(
        &tx,
        "l10n_articles",
        "content",
        &doc.id,
        &[json!({"_block_type": "paragraph", "text": "New English"})],
        Some("en"),
    )
    .unwrap();
    tx.commit().expect("Commit");

    let conn = pool.get().expect("conn");
    let en =
        query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("en")).unwrap();
    assert_eq!(en.len(), 1);
    assert_eq!(en[0]["text"], "New English");

    let de =
        query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("de")).unwrap();
    assert_eq!(de.len(), 1);
    assert_eq!(de[0]["text"], "Deutsch", "DE blocks should be preserved");
}

// ── High-level: save_join_table_data + hydrate_document with locale ──────────

#[test]
fn save_join_table_data_with_locale_scopes_writes() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Save EN join data
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("tags".to_string(), json!(["en-tag"]));
    en_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "English content"}
        ]),
    );
    en_join.insert(
        "meta".to_string(),
        json!([
            {"_block_type": "kv", "key": "shared-meta"}
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();

    // Save DE join data
    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert("tags".to_string(), json!(["de-tag"]));
    de_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "German content"}
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &de_join,
        Some(&de_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Verify EN data
    let conn = pool.get().expect("conn");
    let en_tags =
        query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("en")).unwrap();
    assert_eq!(en_tags, vec!["en-tag"]);
    let en_blocks =
        query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("en")).unwrap();
    assert_eq!(en_blocks.len(), 1);
    assert_eq!(en_blocks[0]["text"], "English content");

    // Verify DE data
    let de_tags =
        query::find_related_ids(&conn, "l10n_articles", "tags", &doc.id, Some("de")).unwrap();
    assert_eq!(de_tags, vec!["de-tag"]);
    let de_blocks =
        query::find_block_rows(&conn, "l10n_articles", "content", &doc.id, Some("de")).unwrap();
    assert_eq!(de_blocks.len(), 1);
    assert_eq!(de_blocks[0]["text"], "German content");

    // Non-localized "meta" field should be written without locale scoping —
    // reading with None should return it
    let meta = query::find_block_rows(&conn, "l10n_articles", "meta", &doc.id, None).unwrap();
    assert_eq!(
        meta.len(),
        1,
        "Non-localized blocks should work without locale"
    );
    assert_eq!(meta[0]["key"], "shared-meta");
}

#[test]
fn hydrate_document_with_locale_returns_correct_data() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN data
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("tags".to_string(), json!(["en-tag-1", "en-tag-2"]));
    en_join.insert(
        "links".to_string(),
        json!([
            {"url": "https://en.example.com", "label": "English"}
        ]),
    );
    en_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "Hello"}
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();

    // Write DE data
    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert("tags".to_string(), json!(["de-tag-1"]));
    de_join.insert(
        "links".to_string(),
        json!([
            {"url": "https://de.example.com", "label": "Deutsch"}
        ]),
    );
    de_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "Hallo"}
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &de_join,
        Some(&de_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Hydrate with EN locale
    let conn = pool.get().expect("conn");
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut en_doc,
        None,
        Some(&en_ctx),
    )
    .unwrap();

    let en_tags = en_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(en_tags.len(), 2);
    assert_eq!(en_tags[0], "en-tag-1");
    assert_eq!(en_tags[1], "en-tag-2");

    let en_links = en_doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(en_links.len(), 1);
    assert_eq!(en_links[0]["label"], "English");

    let en_content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(en_content.len(), 1);
    assert_eq!(en_content[0]["text"], "Hello");

    // Hydrate with DE locale
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let de_tags = de_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(de_tags.len(), 1);
    assert_eq!(de_tags[0], "de-tag-1");

    let de_links = de_doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(de_links.len(), 1);
    assert_eq!(de_links[0]["label"], "Deutsch");

    let de_content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(de_content.len(), 1);
    assert_eq!(de_content[0]["text"], "Hallo");
}

#[test]
fn save_join_data_in_one_locale_does_not_clobber_other() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN content first
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "English paragraph 1"},
            {"_block_type": "paragraph", "text": "English paragraph 2"},
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();

    // Now write DE content — this is the bug scenario: previously this would DELETE all rows
    // (regardless of locale) and then INSERT only the DE rows, destroying EN content.
    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "German paragraph"},
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &de_join,
        Some(&de_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // The critical assertion: EN content must still be intact
    let conn = pool.get().expect("conn");
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut en_doc,
        None,
        Some(&en_ctx),
    )
    .unwrap();

    let en_content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(
        en_content.len(),
        2,
        "English blocks must survive German write"
    );
    assert_eq!(en_content[0]["text"], "English paragraph 1");
    assert_eq!(en_content[1]["text"], "English paragraph 2");

    // And DE content should be correct too
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let de_content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(de_content.len(), 1);
    assert_eq!(de_content[0]["text"], "German paragraph");
}

#[test]
fn non_localized_join_field_ignores_locale_context() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write to the non-localized "meta" blocks field via save_join_table_data with a locale ctx.
    // Since meta.localized=false, the locale should be ignored (writes without _locale scoping).
    let mut join: HashMap<String, serde_json::Value> = HashMap::new();
    join.insert(
        "meta".to_string(),
        json!([
            {"_block_type": "kv", "key": "version"},
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &join,
        Some(&en_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Reading with None should find the data (no locale filter)
    let conn = pool.get().expect("conn");
    let meta = query::find_block_rows(&conn, "l10n_articles", "meta", &doc.id, None).unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0]["key"], "version");

    // Hydrate with a different locale context — non-localized field should still appear
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };
    let mut doc2 = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut doc2,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let meta2 = doc2.get("meta").unwrap().as_array().unwrap();
    assert_eq!(
        meta2.len(),
        1,
        "Non-localized field should be visible regardless of locale context"
    );
}

#[test]
fn hydrate_without_locale_returns_all_locale_rows() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write blocks in both locales
    query::set_block_rows(
        &tx,
        "l10n_articles",
        "content",
        &doc.id,
        &[json!({"_block_type": "paragraph", "text": "EN"})],
        Some("en"),
    )
    .unwrap();
    query::set_block_rows(
        &tx,
        "l10n_articles",
        "content",
        &doc.id,
        &[json!({"_block_type": "paragraph", "text": "DE"})],
        Some("de"),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Hydrate with None locale — should return ALL rows from all locales
    let conn = pool.get().expect("conn");
    let mut doc2 = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(&conn, "l10n_articles", &def.fields, &mut doc2, None, None).unwrap();

    let content = doc2.get("content").unwrap().as_array().unwrap();
    assert_eq!(
        content.len(),
        2,
        "Without locale context, all rows should be returned"
    );

    // Hydrate with EN locale — should return only EN
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut en_doc,
        None,
        Some(&en_ctx),
    )
    .unwrap();
    let en_content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(en_content.len(), 1);
    assert_eq!(en_content[0]["text"], "EN");

    // Hydrate with DE locale — should return only DE
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();
    let de_content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(de_content.len(), 1);
    assert_eq!(de_content[0]["text"], "DE");
}

// ── Locale Fallback on Join Tables ───────────────────────────────────────────
// When fallback=true and requesting a non-default locale that has no data,
// join table reads should fall back to the default locale (matching the
// COALESCE behavior of regular columns).

#[test]
fn join_fallback_has_many_falls_back_to_default_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    assert!(
        locale_config.fallback,
        "Precondition: fallback must be enabled"
    );

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN tags only — no DE tags
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert("tags".to_string(), json!(["tag-1", "tag-2"]));
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE locale — should fall back to EN tags
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let tags = de_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(tags.len(), 2, "DE tags should fall back to EN when empty");
    assert_eq!(tags[0], "tag-1");
    assert_eq!(tags[1], "tag-2");

    // Hydrate with EN locale — should return EN tags directly
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut en_doc,
        None,
        Some(&en_ctx),
    )
    .unwrap();

    let en_tags = en_doc.get("tags").unwrap().as_array().unwrap();
    assert_eq!(en_tags.len(), 2);
}

#[test]
fn join_fallback_array_falls_back_to_default_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN links only
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert(
        "links".to_string(),
        json!([
            {"url": "https://en.example.com", "label": "English Link"}
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE locale — should fall back to EN links
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let links = de_doc.get("links").unwrap().as_array().unwrap();
    assert_eq!(links.len(), 1, "DE links should fall back to EN when empty");
    assert_eq!(links[0]["label"], "English Link");
}

#[test]
fn join_fallback_blocks_falls_back_to_default_locale() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN blocks only
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "English paragraph"}
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE locale — should fall back to EN blocks
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(
        content.len(),
        1,
        "DE blocks should fall back to EN when empty"
    );
    assert_eq!(content[0]["text"], "English paragraph");
}

#[test]
fn join_fallback_does_not_trigger_when_locale_has_data() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write both EN and DE content
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "English"},
            {"_block_type": "paragraph", "text": "More English"},
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();

    let mut de_join: HashMap<String, serde_json::Value> = HashMap::new();
    de_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "Deutsch"},
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &de_join,
        Some(&de_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // DE has its own data — should NOT fall back to EN
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let content = de_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(
        content.len(),
        1,
        "DE has its own data — fallback should NOT trigger"
    );
    assert_eq!(content[0]["text"], "Deutsch");
}

#[test]
fn join_fallback_disabled_returns_empty() {
    let (_tmp, pool, def, mut locale_config) = setup_localized_joins();
    locale_config.fallback = false; // Disable fallback

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN content only
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "English only"}
        ]),
    );
    en_join.insert("tags".to_string(), json!(["en-tag"]));
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Hydrate with DE — with fallback=false, should get empty results
    let conn = pool.get().expect("conn");
    let mut de_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut de_doc,
        None,
        Some(&de_ctx),
    )
    .unwrap();

    let content = de_doc.get("content").unwrap().as_array().unwrap();
    assert!(
        content.is_empty(),
        "With fallback=false, empty DE should NOT fall back to EN"
    );

    let tags = de_doc.get("tags").unwrap().as_array().unwrap();
    assert!(
        tags.is_empty(),
        "With fallback=false, empty DE tags should NOT fall back to EN"
    );
}

#[test]
fn join_fallback_default_locale_no_fallback_needed() {
    let (_tmp, pool, def, locale_config) = setup_localized_joins();
    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = HashMap::new();
    data.insert("slug_field".to_string(), "test".to_string());
    let doc = query::create(&tx, "l10n_articles", &def, &data, None).expect("Create");

    // Write EN content
    let mut en_join: HashMap<String, serde_json::Value> = HashMap::new();
    en_join.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "English"}
        ]),
    );
    query::save_join_table_data(
        &tx,
        "l10n_articles",
        &def.fields,
        &doc.id,
        &en_join,
        Some(&en_ctx),
    )
    .unwrap();
    tx.commit().expect("Commit");

    // Hydrate with EN (default locale) — should return EN data directly, no fallback path
    let conn = pool.get().expect("conn");
    let mut en_doc = query::find_by_id(&conn, "l10n_articles", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    query::hydrate_document(
        &conn,
        "l10n_articles",
        &def.fields,
        &mut en_doc,
        None,
        Some(&en_ctx),
    )
    .unwrap();

    let content = en_doc.get("content").unwrap().as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["text"], "English");
}

// ── Group + Locale Tests (Collections) ────────────────────────────────────────

fn make_localized_group_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages_l10n");
    def.timestamps = true;
    let meta_title = FieldDefinition {
        name: "meta_title".to_string(),
        localized: true,
        ..Default::default()
    };
    let meta_description = FieldDefinition {
        name: "meta_description".to_string(),
        localized: true,
        ..Default::default()
    };
    let seo = FieldDefinition {
        name: "seo".to_string(),
        field_type: FieldType::Group,
        fields: vec![meta_title, meta_description],
        ..Default::default()
    };
    def.fields = vec![make_field("title", FieldType::Text), seo];
    def
}

fn locale_config() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

/// Collection: localized group creates seo__meta_title__en, seo__meta_title__de columns.
#[test]
fn collection_localized_group_migration_creates_locale_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_group_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let conn = pool.get().unwrap();
    let columns: HashSet<String> = conn
        .query_all("PRAGMA table_info(pages_l10n)", &[])
        .unwrap()
        .into_iter()
        .filter_map(|row| row.get_string("name").ok())
        .collect();

    assert!(
        columns.contains("seo__meta_title__en"),
        "Should have en locale column for group sub-field"
    );
    assert!(
        columns.contains("seo__meta_title__de"),
        "Should have de locale column for group sub-field"
    );
    assert!(columns.contains("seo__meta_description__en"));
    assert!(columns.contains("seo__meta_description__de"));
    assert!(
        !columns.contains("seo__meta_title"),
        "Should NOT have non-localized group sub-column"
    );
    assert!(
        !columns.contains("seo"),
        "Should NOT have single group column"
    );
}

/// Collection: write and read localized group sub-fields per locale.
#[test]
fn collection_localized_group_write_and_read() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_localized_group_def();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config(),
    };

    // Create with English group data
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test Page".to_string());
    data.insert("seo__meta_title".to_string(), "EN SEO Title".to_string());
    data.insert(
        "seo__meta_description".to_string(),
        "EN SEO Desc".to_string(),
    );

    let conn = pool.get().unwrap();
    let doc = query::create(&conn, "pages_l10n", &def, &data, Some(&en_ctx)).expect("Create");

    // Update with German group data
    let mut de_data = HashMap::new();
    de_data.insert("seo__meta_title".to_string(), "DE SEO Titel".to_string());
    de_data.insert(
        "seo__meta_description".to_string(),
        "DE SEO Beschreibung".to_string(),
    );
    query::update(&conn, "pages_l10n", &def, &doc.id, &de_data, Some(&de_ctx)).expect("Update DE");

    // Read English — hydrated into nested seo object
    let en_doc = query::find_by_id(&conn, "pages_l10n", &def, &doc.id, Some(&en_ctx))
        .unwrap()
        .unwrap();
    let en_seo = en_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(
        en_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("EN SEO Title")
    );
    assert_eq!(
        en_seo.get("meta_description").and_then(|v| v.as_str()),
        Some("EN SEO Desc")
    );

    // Read German — should get DE values
    let de_doc = query::find_by_id(&conn, "pages_l10n", &def, &doc.id, Some(&de_ctx))
        .unwrap()
        .unwrap();
    let de_seo = de_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(
        de_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("DE SEO Titel")
    );
    assert_eq!(
        de_seo.get("meta_description").and_then(|v| v.as_str()),
        Some("DE SEO Beschreibung")
    );
}

// ── Group + Locale Tests (Globals) ────────────────────────────────────────────

fn make_global_with_localized_group() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("site_l10n");
    let meta_title = FieldDefinition {
        name: "meta_title".to_string(),
        localized: true,
        ..Default::default()
    };
    let meta_description = FieldDefinition {
        name: "meta_description".to_string(),
        localized: true,
        ..Default::default()
    };
    let seo = FieldDefinition {
        name: "seo".to_string(),
        field_type: FieldType::Group,
        fields: vec![meta_title, meta_description],
        ..Default::default()
    };
    def.fields = vec![make_field("site_name", FieldType::Text), seo];
    def
}

/// Global: localized group creates seo__meta_title__en, seo__meta_title__de columns.
#[test]
fn global_localized_group_migration_creates_locale_columns() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_with_localized_group();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let conn = pool.get().unwrap();
    let columns: HashSet<String> = conn
        .query_all("PRAGMA table_info(_global_site_l10n)", &[])
        .unwrap()
        .into_iter()
        .filter_map(|row| row.get_string("name").ok())
        .collect();

    assert!(
        columns.contains("seo__meta_title__en"),
        "Should have en locale column for group sub-field"
    );
    assert!(
        columns.contains("seo__meta_title__de"),
        "Should have de locale column for group sub-field"
    );
    assert!(columns.contains("seo__meta_description__en"));
    assert!(columns.contains("seo__meta_description__de"));
    assert!(
        !columns.contains("seo__meta_title"),
        "Should NOT have non-localized group sub-column"
    );
    assert!(
        !columns.contains("seo"),
        "Should NOT have single group column"
    );
}

/// Global: write and read localized group sub-fields per locale.
#[test]
fn global_localized_group_write_and_read() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_with_localized_group();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config(),
    };

    // Update with English group data
    let mut data = HashMap::new();
    data.insert("site_name".to_string(), "My Site".to_string());
    data.insert("seo__meta_title".to_string(), "EN Title".to_string());
    data.insert("seo__meta_description".to_string(), "EN Desc".to_string());

    let conn = pool.get().unwrap();
    query::update_global(&conn, "site_l10n", &def, &data, Some(&en_ctx)).expect("Update EN");

    // Update with German group data
    let mut de_data = HashMap::new();
    de_data.insert("seo__meta_title".to_string(), "DE Titel".to_string());
    de_data.insert(
        "seo__meta_description".to_string(),
        "DE Beschreibung".to_string(),
    );
    query::update_global(&conn, "site_l10n", &def, &de_data, Some(&de_ctx)).expect("Update DE");

    // Read English (hydrated into nested seo object)
    let en_doc = query::get_global(&conn, "site_l10n", &def, Some(&en_ctx)).expect("Get EN");
    let en_seo = en_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(
        en_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("EN Title")
    );
    assert_eq!(
        en_seo.get("meta_description").and_then(|v| v.as_str()),
        Some("EN Desc")
    );

    // Read German
    let de_doc = query::get_global(&conn, "site_l10n", &def, Some(&de_ctx)).expect("Get DE");
    let de_seo = de_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(
        de_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("DE Titel")
    );
    assert_eq!(
        de_seo.get("meta_description").and_then(|v| v.as_str()),
        Some("DE Beschreibung")
    );
}

/// Global: locale fallback for group sub-fields.
#[test]
fn global_localized_group_fallback() {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_global_with_localized_group();
    {
        let mut reg = registry.write().unwrap();
        reg.register_global(def.clone());
    }
    migrate::sync_all(&pool, &registry, &locale_config()).expect("Sync");

    let en_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config(),
    };
    let de_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("de".to_string()),
        config: locale_config(),
    };

    // Only set English data
    let mut data = HashMap::new();
    data.insert("seo__meta_title".to_string(), "EN Title".to_string());
    let conn = pool.get().unwrap();
    query::update_global(&conn, "site_l10n", &def, &data, Some(&en_ctx)).expect("Update EN");

    // Read German — should fall back to English (fallback=true)
    let de_doc = query::get_global(&conn, "site_l10n", &def, Some(&de_ctx)).expect("Get DE");
    let de_seo = de_doc.fields.get("seo").expect("seo should exist");
    assert_eq!(
        de_seo.get("meta_title").and_then(|v| v.as_str()),
        Some("EN Title"),
        "Should fall back to EN"
    );
}
