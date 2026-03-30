use std::collections::{HashMap, HashSet};

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Registry;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};
use crap_cms::db::{migrate, pool, query};
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

// ── 1C. Join Tables ──────────────────────────────────────────────────────────

fn make_articles_with_join_tables() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.timestamps = true;
    def.fields = vec![
        make_field("title", FieldType::Text),
        // has-many relationship
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build(),
        // array field with sub-fields
        FieldDefinition::builder("links", FieldType::Array)
            .fields(vec![
                make_field("url", FieldType::Text),
                make_field("label", FieldType::Text),
            ])
            .build(),
        // blocks field
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("paragraph", vec![make_field("text", FieldType::Textarea)]),
                BlockDefinition::new("image", vec![make_field("url", FieldType::Text)]),
            ])
            .build(),
    ];
    def
}

fn setup_articles() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let registry = Registry::shared();
    let def = make_articles_with_join_tables();
    let mut tags_def = CollectionDefinition::new("tags");
    tags_def.timestamps = true;
    tags_def.fields = vec![make_field("name", FieldType::Text)];
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_collection(tags_def);
    }
    migrate::sync_all(&pool, &registry, &CrapConfig::default().locale).expect("Sync failed");
    (_tmp, pool, def)
}

#[test]
fn set_and_find_related_ids() {
    let (_tmp, pool, def) = setup_articles();

    // Create an article
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test Article".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let ids = vec![
        "tag-1".to_string(),
        "tag-2".to_string(),
        "tag-3".to_string(),
    ];
    query::set_related_ids(&tx, "articles", "tags", &doc.id, &ids, None)
        .expect("Set related ids failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
        .expect("Find related ids failed");
    assert_eq!(found, ids);
}

#[test]
fn set_related_ids_replaces_existing() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // First set
    query::set_related_ids(
        &tx,
        "articles",
        "tags",
        &doc.id,
        &["a".to_string(), "b".to_string()],
        None,
    )
    .expect("Set failed");

    // Replace
    query::set_related_ids(
        &tx,
        "articles",
        "tags",
        &doc.id,
        &["c".to_string(), "d".to_string()],
        None,
    )
    .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found =
        query::find_related_ids(&conn, "articles", "tags", &doc.id, None).expect("Find failed");
    assert_eq!(found, vec!["c".to_string(), "d".to_string()]);
}

#[test]
fn find_related_ids_empty() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found =
        query::find_related_ids(&conn, "articles", "tags", &doc.id, None).expect("Find failed");
    assert!(found.is_empty());
}

#[test]
fn set_and_find_array_rows() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let sub_fields = &links_field.fields;

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let rows = vec![
        {
            let mut m = HashMap::new();
            m.insert("url".to_string(), "https://example.com".to_string());
            m.insert("label".to_string(), "Example".to_string());
            m
        },
        {
            let mut m = HashMap::new();
            m.insert("url".to_string(), "https://rust-lang.org".to_string());
            m.insert("label".to_string(), "Rust".to_string());
            m
        },
    ];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows, sub_fields, None)
        .expect("Set array rows failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields, None)
        .expect("Find array rows failed");
    assert_eq!(found.len(), 2);
    assert_eq!(
        found[0].get("url").unwrap().as_str().unwrap(),
        "https://example.com"
    );
    assert_eq!(found[0].get("label").unwrap().as_str().unwrap(), "Example");
    assert_eq!(
        found[1].get("url").unwrap().as_str().unwrap(),
        "https://rust-lang.org"
    );
    // Each row should have an id
    assert!(found[0].get("id").is_some());
}

#[test]
fn set_array_rows_replaces_existing() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let sub_fields = &links_field.fields;

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let rows1 = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://old.com".to_string());
        m.insert("label".to_string(), "Old".to_string());
        m
    }];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows1, sub_fields, None)
        .expect("Set failed");

    let rows2 = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://new.com".to_string());
        m.insert("label".to_string(), "New".to_string());
        m
    }];
    query::set_array_rows(&tx, "articles", "links", &doc.id, &rows2, sub_fields, None)
        .expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields, None)
        .expect("Find failed");
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].get("url").unwrap().as_str().unwrap(),
        "https://new.com"
    );
}

#[test]
fn find_array_rows_empty() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let sub_fields = &links_field.fields;

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_array_rows(&conn, "articles", "links", &doc.id, sub_fields, None)
        .expect("Find failed");
    assert!(found.is_empty());
}

#[test]
fn set_and_find_block_rows() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let blocks = vec![
        json!({"_block_type": "paragraph", "text": "Hello world"}),
        json!({"_block_type": "image", "url": "/img/test.png"}),
    ];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks, None)
        .expect("Set block rows failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found = query::find_block_rows(&conn, "articles", "content", &doc.id, None)
        .expect("Find block rows failed");
    assert_eq!(found.len(), 2);
    assert_eq!(
        found[0].get("_block_type").unwrap().as_str().unwrap(),
        "paragraph"
    );
    assert_eq!(
        found[0].get("text").unwrap().as_str().unwrap(),
        "Hello world"
    );
    assert_eq!(
        found[1].get("_block_type").unwrap().as_str().unwrap(),
        "image"
    );
    assert_eq!(
        found[1].get("url").unwrap().as_str().unwrap(),
        "/img/test.png"
    );
    // Each block should have an id
    assert!(found[0].get("id").is_some());
}

#[test]
fn set_block_rows_replaces_existing() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    let blocks1 = vec![json!({"_block_type": "paragraph", "text": "Old"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks1, None).expect("Set failed");

    let blocks2 = vec![json!({"_block_type": "image", "url": "/new.png"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks2, None).expect("Set failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found =
        query::find_block_rows(&conn, "articles", "content", &doc.id, None).expect("Find failed");
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].get("_block_type").unwrap().as_str().unwrap(),
        "image"
    );
}

#[test]
fn find_block_rows_empty() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let found =
        query::find_block_rows(&conn, "articles", "content", &doc.id, None).expect("Find failed");
    assert!(found.is_empty());
}

#[test]
fn hydrate_document_populates_join_data() {
    let (_tmp, pool, def) = setup_articles();
    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // Set up join table data
    query::set_related_ids(
        &tx,
        "articles",
        "tags",
        &doc.id,
        &["t1".to_string(), "t2".to_string()],
        None,
    )
    .expect("Set related failed");
    let rows = vec![{
        let mut m = HashMap::new();
        m.insert("url".to_string(), "https://example.com".to_string());
        m.insert("label".to_string(), "Ex".to_string());
        m
    }];
    query::set_array_rows(
        &tx,
        "articles",
        "links",
        &doc.id,
        &rows,
        &links_field.fields,
        None,
    )
    .expect("Set array failed");
    let blocks = vec![json!({"_block_type": "paragraph", "text": "Hi"})];
    query::set_block_rows(&tx, "articles", "content", &doc.id, &blocks, None)
        .expect("Set blocks failed");
    tx.commit().expect("Commit");

    // Hydrate
    let conn = pool.get().expect("DB connection");
    let mut doc = query::find_by_id(&conn, "articles", &def, &doc.id, None)
        .expect("Find failed")
        .expect("Not found");
    query::hydrate_document(&conn, "articles", &def.fields, &mut doc, None, None)
        .expect("Hydrate failed");

    // Verify tags (has-many relationship)
    let tags = doc.get("tags").expect("tags should exist");
    assert!(tags.is_array());
    let tags_arr = tags.as_array().unwrap();
    assert_eq!(tags_arr.len(), 2);
    assert_eq!(tags_arr[0].as_str().unwrap(), "t1");

    // Verify links (array)
    let links = doc.get("links").expect("links should exist");
    assert!(links.is_array());
    let links_arr = links.as_array().unwrap();
    assert_eq!(links_arr.len(), 1);
    assert_eq!(
        links_arr[0].get("url").unwrap().as_str().unwrap(),
        "https://example.com"
    );

    // Verify content (blocks)
    let content = doc.get("content").expect("content should exist");
    assert!(content.is_array());
    let blocks_arr = content.as_array().unwrap();
    assert_eq!(blocks_arr.len(), 1);
    assert_eq!(
        blocks_arr[0].get("_block_type").unwrap().as_str().unwrap(),
        "paragraph"
    );
}

#[test]
fn save_join_table_data_from_hashmap() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // Prepare join table data as JSON values
    let mut jt_data: HashMap<String, serde_json::Value> = HashMap::new();
    jt_data.insert("tags".to_string(), json!(["tag-a", "tag-b"]));
    jt_data.insert(
        "links".to_string(),
        json!([
            {"url": "https://a.com", "label": "A"},
            {"url": "https://b.com", "label": "B"},
        ]),
    );
    jt_data.insert(
        "content".to_string(),
        json!([
            {"_block_type": "paragraph", "text": "Content block"},
        ]),
    );

    query::save_join_table_data(&tx, "articles", &def.fields, &doc.id, &jt_data, None)
        .expect("Save join table data failed");
    tx.commit().expect("Commit");

    // Verify
    let conn = pool.get().expect("DB connection");
    let tags = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
        .expect("Find tags failed");
    assert_eq!(tags, vec!["tag-a", "tag-b"]);

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links = query::find_array_rows(
        &conn,
        "articles",
        "links",
        &doc.id,
        &links_field.fields,
        None,
    )
    .expect("Find links failed");
    assert_eq!(links.len(), 2);

    let blocks = query::find_block_rows(&conn, "articles", "content", &doc.id, None)
        .expect("Find blocks failed");
    assert_eq!(blocks.len(), 1);
}

#[test]
fn save_join_table_data_partial_update() {
    let (_tmp, pool, def) = setup_articles();
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut data = HashMap::new();
    data.insert("title".to_string(), "Test".to_string());
    let doc = query::create(&tx, "articles", &def, &data, None).expect("Create failed");

    // First: set tags and links
    let mut jt_data: HashMap<String, serde_json::Value> = HashMap::new();
    jt_data.insert("tags".to_string(), json!(["tag-1", "tag-2"]));
    jt_data.insert(
        "links".to_string(),
        json!([{"url": "https://a.com", "label": "A"}]),
    );
    query::save_join_table_data(&tx, "articles", &def.fields, &doc.id, &jt_data, None)
        .expect("Save failed");

    // Second: only update tags (links should be unchanged)
    let mut jt_data2: HashMap<String, serde_json::Value> = HashMap::new();
    jt_data2.insert("tags".to_string(), json!(["tag-3"]));
    query::save_join_table_data(&tx, "articles", &def.fields, &doc.id, &jt_data2, None)
        .expect("Save failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let tags = query::find_related_ids(&conn, "articles", "tags", &doc.id, None)
        .expect("Find tags failed");
    assert_eq!(tags, vec!["tag-3"]);

    let links_field = def.fields.iter().find(|f| f.name == "links").unwrap();
    let links = query::find_array_rows(
        &conn,
        "articles",
        "links",
        &doc.id,
        &links_field.fields,
        None,
    )
    .expect("Find links failed");
    // Links should be unchanged (not in the second update)
    assert_eq!(links.len(), 1);
}

// ── 1D. Relationship Population / Depth ───────────────────────────────────────

fn make_categories_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("categories");
    def.timestamps = true;
    def.fields = vec![
        make_field("name", FieldType::Text),
        // Self-referencing parent (for circular ref test)
        FieldDefinition::builder("parent", FieldType::Relationship)
            .relationship(RelationshipConfig::new("categories", false))
            .build(),
    ];
    def
}

fn make_posts_with_category() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts_v2");
    def.timestamps = true;
    def.fields = vec![
        make_field("title", FieldType::Text),
        // has-one relationship to categories
        FieldDefinition::builder("category", FieldType::Relationship)
            .relationship(RelationshipConfig::new("categories", false))
            .build(),
        // has-many relationship to categories
        FieldDefinition::builder("secondary_categories", FieldType::Relationship)
            .relationship(RelationshipConfig::new("categories", true))
            .build(),
        // field with max_depth cap
        FieldDefinition::builder("limited_cat", FieldType::Relationship)
            .relationship({
                let mut rc = RelationshipConfig::new("categories", false);
                rc.max_depth = Some(0);
                rc
            })
            .build(),
    ];
    def
}

fn setup_posts_categories() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    crap_cms::core::SharedRegistry,
    CollectionDefinition,
    CollectionDefinition,
) {
    let (_tmp, pool) = create_test_pool();
    let shared_registry = Registry::shared();
    let cats_def = make_categories_def();
    let posts_def = make_posts_with_category();
    {
        let mut reg = shared_registry.write().unwrap();
        reg.register_collection(cats_def.clone());
        reg.register_collection(posts_def.clone());
    }
    migrate::sync_all(&pool, &shared_registry, &CrapConfig::default().locale).expect("Sync failed");

    (_tmp, pool, shared_registry, posts_def, cats_def)
}

#[test]
fn populate_depth_0_leaves_ids() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    // Create a category
    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut cat_data = HashMap::new();
    cat_data.insert("name".to_string(), "Tech".to_string());
    let cat =
        query::create(&tx, "categories", &cats_def, &cat_data, None).expect("Create cat failed");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "My Post".to_string());
    post_data.insert("category".to_string(), cat.id.to_string());
    let mut post =
        query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post failed");
    tx.commit().expect("Commit");

    // Populate at depth 0 — should be a no-op
    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(
        &query::PopulateContext::new(&conn, &registry.read().unwrap(), "posts_v2", &posts_def),
        &mut post,
        &mut visited,
        &query::PopulateOpts::new(0),
    )
    .expect("Populate failed");

    // category should still be an ID string
    assert_eq!(post.get_str("category"), Some(cat.id.as_ref()));
}

#[test]
fn populate_depth_1_hydrates_has_one() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut cat_data = HashMap::new();
    cat_data.insert("name".to_string(), "Tech".to_string());
    let cat = query::create(&tx, "categories", &cats_def, &cat_data, None).expect("Create cat");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "My Post".to_string());
    post_data.insert("category".to_string(), cat.id.to_string());
    let mut post =
        query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(
        &query::PopulateContext::new(&conn, &registry.read().unwrap(), "posts_v2", &posts_def),
        &mut post,
        &mut visited,
        &query::PopulateOpts::new(1),
    )
    .expect("Populate failed");

    // category should be a full document object
    let cat_val = post.get("category").expect("category should exist");
    assert!(
        cat_val.is_object(),
        "category should be an object, got: {:?}",
        cat_val
    );
    assert_eq!(cat_val.get("name").unwrap().as_str().unwrap(), "Tech");
    assert_eq!(
        cat_val.get("id").unwrap().as_str().unwrap(),
        cat.id.as_ref()
    );
}

#[test]
fn populate_depth_1_hydrates_has_many() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let mut cat1_data = HashMap::new();
    cat1_data.insert("name".to_string(), "Tech".to_string());
    let cat1 = query::create(&tx, "categories", &cats_def, &cat1_data, None).expect("Create cat1");

    let mut cat2_data = HashMap::new();
    cat2_data.insert("name".to_string(), "Science".to_string());
    let cat2 = query::create(&tx, "categories", &cats_def, &cat2_data, None).expect("Create cat2");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "Multi-cat Post".to_string());
    let mut post =
        query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");

    query::set_related_ids(
        &tx,
        "posts_v2",
        "secondary_categories",
        &post.id,
        &[cat1.id.to_string(), cat2.id.to_string()],
        None,
    )
    .expect("Set related failed");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    query::hydrate_document(&conn, "posts_v2", &posts_def.fields, &mut post, None, None)
        .expect("Hydrate failed");

    let mut visited = HashSet::new();
    query::populate_relationships(
        &query::PopulateContext::new(&conn, &registry.read().unwrap(), "posts_v2", &posts_def),
        &mut post,
        &mut visited,
        &query::PopulateOpts::new(1),
    )
    .expect("Populate failed");

    let sec_cats = post.get("secondary_categories").expect("should exist");
    assert!(sec_cats.is_array());
    let arr = sec_cats.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Should be full objects, not IDs
    assert!(arr[0].is_object());
    assert!(arr[0].get("name").is_some());
}

#[test]
fn populate_circular_ref_stops() {
    let (_tmp, pool, registry, _posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    // Create cat A → parent B → parent A (circular)
    let mut a_data = HashMap::new();
    a_data.insert("name".to_string(), "A".to_string());
    let cat_a = query::create(&tx, "categories", &cats_def, &a_data, None).expect("Create A");

    let mut b_data = HashMap::new();
    b_data.insert("name".to_string(), "B".to_string());
    b_data.insert("parent".to_string(), cat_a.id.to_string());
    let cat_b = query::create(&tx, "categories", &cats_def, &b_data, None).expect("Create B");

    // Update A to point to B
    let mut update = HashMap::new();
    update.insert("parent".to_string(), cat_b.id.to_string());
    let mut cat_a =
        query::update(&tx, "categories", &cats_def, &cat_a.id, &update, None).expect("Update A");
    tx.commit().expect("Commit");

    // Populate at depth 10 — should not infinite loop
    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(
        &query::PopulateContext::new(&conn, &registry.read().unwrap(), "categories", &cats_def),
        &mut cat_a,
        &mut visited,
        &query::PopulateOpts::new(10),
    )
    .expect("Populate should not loop");
    // Should complete without panic
}

#[test]
fn populate_missing_related_doc() {
    let (_tmp, pool, registry, posts_def, _cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");
    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "Orphaned".to_string());
    post_data.insert("category".to_string(), "nonexistent-cat-id".to_string());
    let mut post =
        query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    query::populate_relationships(
        &query::PopulateContext::new(&conn, &registry.read().unwrap(), "posts_v2", &posts_def),
        &mut post,
        &mut visited,
        &query::PopulateOpts::new(1),
    )
    .expect("Populate should handle missing");

    // Category should remain as a string ID (not populated)
    assert_eq!(post.get_str("category"), Some("nonexistent-cat-id"));
}

#[test]
fn populate_respects_field_max_depth() {
    let (_tmp, pool, registry, posts_def, cats_def) = setup_posts_categories();

    let mut conn = pool.get().expect("DB connection");
    let tx = conn.transaction().expect("Start transaction");

    let mut cat_data = HashMap::new();
    cat_data.insert("name".to_string(), "Tech".to_string());
    let cat = query::create(&tx, "categories", &cats_def, &cat_data, None).expect("Create cat");

    let mut post_data = HashMap::new();
    post_data.insert("title".to_string(), "Post".to_string());
    post_data.insert("limited_cat".to_string(), cat.id.to_string());
    let mut post =
        query::create(&tx, "posts_v2", &posts_def, &post_data, None).expect("Create post");
    tx.commit().expect("Commit");

    let conn = pool.get().expect("DB connection");
    let mut visited = HashSet::new();
    // Even with depth=5, the limited_cat field has max_depth=0, so it shouldn't populate
    query::populate_relationships(
        &query::PopulateContext::new(&conn, &registry.read().unwrap(), "posts_v2", &posts_def),
        &mut post,
        &mut visited,
        &query::PopulateOpts::new(5),
    )
    .expect("Populate failed");

    // limited_cat should remain as string ID (max_depth=0 prevents population)
    assert_eq!(post.get_str("limited_cat"), Some(cat.id.as_ref()));
}

// Regression: populate_relationships with localized fields on the related collection
// used to fail because find_by_ids was called without locale_ctx, generating
// `SELECT caption` instead of `SELECT caption__en` for localized columns.
#[test]
fn populate_with_localized_related_collection() {
    let (_tmp, pool) = create_test_pool();
    let shared_registry = Registry::shared();

    // "media" collection with a localized field
    let mut media_def = CollectionDefinition::new("media");
    media_def.timestamps = true;
    media_def.fields = vec![
        make_field("url", FieldType::Text),
        FieldDefinition::builder("caption", FieldType::Text)
            .localized(true)
            .build(),
    ];

    // "articles" collection with a relationship to media
    let mut articles_def = CollectionDefinition::new("articles");
    articles_def.timestamps = true;
    articles_def.fields = vec![
        make_field("title", FieldType::Text),
        FieldDefinition::builder("image", FieldType::Relationship)
            .relationship(RelationshipConfig::new("media", false))
            .build(),
    ];

    {
        let mut reg = shared_registry.write().unwrap();
        reg.register_collection(media_def.clone());
        reg.register_collection(articles_def.clone());
    }

    let locale_config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    migrate::sync_all(&pool, &shared_registry, &locale_config).expect("Sync failed");

    let locale_ctx = query::LocaleContext {
        mode: query::LocaleMode::Single("en".to_string()),
        config: locale_config.clone(),
    };

    // Create a media document with localized caption
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut media_data = HashMap::new();
    media_data.insert("url".to_string(), "/img/test.png".to_string());
    media_data.insert("caption".to_string(), "Test image".to_string());
    let media_doc = query::create(&tx, "media", &media_def, &media_data, Some(&locale_ctx))
        .expect("Create media");

    // Create an article referencing the media
    let mut article_data = HashMap::new();
    article_data.insert("title".to_string(), "My Article".to_string());
    article_data.insert("image".to_string(), media_doc.id.to_string());
    let mut article =
        query::create(&tx, "articles", &articles_def, &article_data, None).expect("Create article");
    tx.commit().expect("Commit");

    // Populate at depth 1 WITH locale_ctx — this used to fail with
    // "Failed to prepare find_by_ids query on 'media'" because the populate
    // code didn't forward locale_ctx to find_by_ids.
    let conn = pool.get().expect("conn");
    let mut visited = HashSet::new();
    query::populate_relationships(
        &query::PopulateContext::new(
            &conn,
            &shared_registry.read().unwrap(),
            "articles",
            &articles_def,
        ),
        &mut article,
        &mut visited,
        &query::PopulateOpts::new(1).locale_ctx(&locale_ctx),
    )
    .expect("Populate with localized related collection should succeed");

    // image should be populated as a full object
    let img = article.get("image").expect("image field should exist");
    assert!(
        img.is_object(),
        "image should be populated object, got: {:?}",
        img
    );
    assert_eq!(img.get("url").unwrap().as_str().unwrap(), "/img/test.png");
    assert_eq!(img.get("caption").unwrap().as_str().unwrap(), "Test image");
}
