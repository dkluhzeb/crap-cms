//! A join lists the documents whose `on` field holds this document — so it
//! lists only those whose `on` field the reader may read. Being listed would
//! otherwise reveal the value the read strip removed from each child.

use std::{path::PathBuf, sync::Arc};

use crap_cms::{
    config::CrapConfig,
    core::{
        Document, HookRef, JoinConfig, Registry, RelationshipConfig,
        collection::CollectionDefinition,
        field::{FieldAccess, FieldDefinition, FieldType},
    },
    db::{DbConnection, DbPool, FindQuery, migrate, pool},
    hooks::lifecycle::HookRunner,
    service::{
        FindByIdInput, FindDocumentsInput, RunnerReadHooks, ServiceContext, find_document_by_id,
        find_documents,
    },
};
use serde_json::{Value, json};

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    registry: Arc<Registry>,
}

/// `writers` joins `pieces` on `writer`; `pieces.writer` is readable by
/// admins only.
fn definitions() -> (CollectionDefinition, CollectionDefinition) {
    let mut writers = CollectionDefinition::new("writers");
    writers.fields = vec![
        FieldDefinition::builder("name", FieldType::Text).build(),
        FieldDefinition::builder("pieces", FieldType::Join)
            .join(JoinConfig::new("pieces", "writer"))
            .build(),
    ];

    let mut pieces = CollectionDefinition::new("pieces");
    pieces.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("writer", FieldType::Relationship)
            .relationship(RelationshipConfig::new("writers", false))
            .access(FieldAccess {
                read: Some(HookRef::new("access.admin_only")),
                ..Default::default()
            })
            .build(),
    ];

    (writers, pieces)
}

/// `writers` joins `pieces` on `writer`, listing at most two; `pieces.writer`
/// is readable only on a piece whose `public` flag is set — a rule that needs
/// each child to decide.
fn limited_definitions() -> (CollectionDefinition, CollectionDefinition) {
    let mut join = JoinConfig::new("pieces", "writer");
    join.limit = Some(2);

    let mut writers = CollectionDefinition::new("writers");
    writers.fields = vec![
        FieldDefinition::builder("name", FieldType::Text).build(),
        FieldDefinition::builder("pieces", FieldType::Join)
            .join(join)
            .build(),
    ];

    let mut pieces = CollectionDefinition::new("pieces");
    pieces.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("public", FieldType::Checkbox).build(),
        FieldDefinition::builder("writer", FieldType::Relationship)
            .relationship(RelationshipConfig::new("writers", false))
            .access(FieldAccess {
                read: Some(HookRef::new("access.field_read_if_public")),
                ..Default::default()
            })
            .build(),
    ];

    (writers, pieces)
}

fn setup() -> Harness {
    setup_with(
        definitions(),
        &[
            "INSERT INTO writers (id, name) VALUES ('w1', 'Ada')",
            "INSERT INTO pieces (id, title, writer) VALUES ('p1', 'One', 'w1')",
            "INSERT INTO pieces (id, title, writer) VALUES ('p2', 'Two', 'w1')",
        ],
    )
}

fn setup_with(
    (writers, pieces): (CollectionDefinition, CollectionDefinition),
    seed: &[&str],
) -> Harness {
    // The example config dir supplies the `access.*` hook modules.
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(writers);
        reg.register_collection(pieces);
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let conn = pool.get().unwrap();
    for sql in seed {
        conn.execute(sql, &[]).expect("seed");
    }
    drop(conn);

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    Harness {
        _tmp: tmp,
        pool,
        runner,
        registry,
    }
}

fn admin() -> Document {
    let mut doc = Document::new("admin-1".to_string());
    doc.fields.insert("role".into(), json!("admin"));
    doc
}

/// The ids a populated `pieces` join holds.
fn joined_ids(pieces: Option<&Value>) -> Vec<String> {
    let mut ids: Vec<String> = pieces
        .and_then(Value::as_array)
        .expect("a populated join array")
        .iter()
        .filter_map(|child| child.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    ids.sort();

    ids
}

/// `w1`'s join read by id at depth 1 — the single-document populate.
fn by_id_join(h: &Harness, user: Option<&Document>) -> Vec<String> {
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, user, None);
    let def = h.registry.get_collection("writers").unwrap();
    let ctx = ServiceContext::collection("writers", def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user)
        .registry(Some(h.registry.as_ref()))
        .build();

    let doc = find_document_by_id(&ctx, &FindByIdInput::builder("w1").depth(1).build())
        .expect("read")
        .expect("document");

    joined_ids(doc.fields.get("pieces"))
}

/// `w1`'s join read through a list at depth 1 — the batched populate.
fn list_join(h: &Harness, user: Option<&Document>) -> Vec<String> {
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, user, None);
    let def = h.registry.get_collection("writers").unwrap();
    let ctx = ServiceContext::collection("writers", def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user)
        .registry(Some(h.registry.as_ref()))
        .build();

    let fq = FindQuery::builder().build();
    let result =
        find_documents(&ctx, &FindDocumentsInput::builder(&fq).depth(1).build()).expect("list");

    let writer = result.docs.first().expect("w1 listed");

    joined_ids(writer.fields.get("pieces"))
}

/// Regression: the join listed `p1` and `p2` for a reader who may not read
/// `pieces.writer` — the strip removed `writer` from each piece, but the
/// pieces' presence under `w1` still said their writer is `w1`.
#[test]
fn a_join_hides_children_whose_on_field_the_reader_cannot_read() {
    let h = setup();

    assert!(by_id_join(&h, None).is_empty());
    assert!(list_join(&h, None).is_empty());
}

#[test]
fn a_join_lists_children_whose_on_field_the_reader_can_read() {
    let h = setup();
    let admin = admin();
    let both = vec!["p1".to_string(), "p2".to_string()];

    assert_eq!(by_id_join(&h, Some(&admin)), both);
    assert_eq!(list_join(&h, Some(&admin)), both);
}

/// Regression: the join's `limit` was applied in SQL before the children
/// whose `on` value the reader may not read were dropped, so with the five
/// newest pieces private the join listed nothing although three public pieces
/// exist. The limit now counts listed children, on the single-document and
/// the batched populate alike: the two newest public pieces.
#[test]
fn a_join_limit_counts_only_children_whose_on_value_is_readable() {
    let mut seed = vec!["INSERT INTO writers (id, name) VALUES ('w1', 'Ada')".to_string()];
    for (id, public, day) in [
        ("p1", 0, 10),
        ("p2", 0, 9),
        ("p3", 0, 8),
        ("p4", 0, 7),
        ("p5", 0, 6),
        ("p6", 1, 3),
        ("p7", 1, 2),
        ("p8", 1, 1),
    ] {
        seed.push(format!(
            "INSERT INTO pieces (id, title, public, writer, created_at, updated_at) \
             VALUES ('{id}', '{id}', {public}, 'w1', '2024-01-{day:02}', '2024-01-{day:02}')"
        ));
    }
    let seed: Vec<&str> = seed.iter().map(String::as_str).collect();

    let h = setup_with(limited_definitions(), &seed);
    let newest_public = vec!["p6".to_string(), "p7".to_string()];

    assert_eq!(by_id_join(&h, None), newest_public);
    assert_eq!(list_join(&h, None), newest_public);
}
