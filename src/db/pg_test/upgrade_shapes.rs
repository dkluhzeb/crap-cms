//! Postgres harness: tables an older release created — case-variant duplicate
//! emails, a junction keyed without `_locale`, inline `UNIQUE` constraints, a
//! junction still holding every locale's rows of a field no longer localized —
//! are brought to the current shape by the schema sync, or reported.

#![cfg(all(test, feature = "postgres"))]

use super::{
    pg_test_pool,
    support::{drop_tables_matching, no_locale, unique_constraints},
    unique_slug,
};
use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig,
        collection::Auth,
    },
    db::{
        DbConnection, DbValue,
        migrate::sync_all,
        query::{find_related_ids, set_related_ids},
    },
};

fn locale_en_de() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

fn registry_of(def: CollectionDefinition) -> Registry {
    let mut registry = Registry::new();
    registry.register_collection(def);

    registry
}

/// Regression: an older release compared emails as typed, so one address
/// could be stored twice in different capitals. The first start stopped on a
/// raw `UNIQUE constraint` error from creating an index — naming no document —
/// before the pass that reports such duplicates ran. The sync now reports each
/// pair with the ids holding it (non-ASCII capitals included) and changes
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_case_variant_duplicate_emails_are_reported_by_document() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let users = unique_slug("f1users");

    {
        let conn = pool.get().expect("conn");
        conn.execute_batch(&format!(
            "CREATE TABLE \"{users}\" (id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, \
             _password_hash TEXT, created_at TEXT, updated_at TEXT); \
             INSERT INTO \"{users}\" (id, email) VALUES ('u1', 'Bob@x.com'), \
             ('u2', 'bob@x.com'), ('u3', '\u{c4}rger@x.com'), ('u4', '\u{e4}rger@x.com');"
        ))
        .unwrap();
    }

    let mut def = CollectionDefinition::new(users.as_str());
    def.auth = Some(Auth::new(true));
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
    ];

    let err = sync_all(&pool, &registry_of(def), &no_locale())
        .expect_err("duplicate accounts stop the start");
    let msg = format!("{err:#}");

    assert!(msg.contains("canonical form"), "{msg}");
    assert!(msg.contains("u1, u2"), "{msg}");
    assert!(msg.contains("u3, u4"), "{msg}");

    let conn = pool.get().expect("conn");
    drop_tables_matching(&conn, &users);
}

fn tagged_posts(posts: &str, tags: &str, localized: bool) -> Registry {
    let mut def = CollectionDefinition::new(posts);
    def.fields = vec![
        FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(localized)
            .relationship(RelationshipConfig::new(tags, true))
            .build(),
    ];

    let mut registry = registry_of(def);
    registry.register_collection(CollectionDefinition::new(tags));

    registry
}

/// Regression: a has-many junction that gained `_locale` after it existed
/// kept its `(parent_id, related_id)` key, so the second locale's list could
/// not name a document the first one held. The sync re-keys it in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_junction_turned_localized_holds_the_same_id_per_locale() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("f2posts");
    let tags = unique_slug("f2tags");

    sync_all(&pool, &tagged_posts(&posts, &tags, false), &no_locale()).expect("sync");

    {
        let conn = pool.get().expect("conn");
        conn.execute(&format!("INSERT INTO \"{posts}\" (id) VALUES ('p1')"), &[])
            .unwrap();
        set_related_ids(&conn, &posts, "tags", "p1", &["t1".to_string()], None).unwrap();
    }

    sync_all(&pool, &tagged_posts(&posts, &tags, true), &locale_en_de()).expect("localized");

    let conn = pool.get().expect("conn");
    set_related_ids(&conn, &posts, "tags", "p1", &["t1".to_string()], Some("de"))
        .expect("the same id in a second locale is a distinct row");

    for locale in ["en", "de"] {
        assert_eq!(
            find_related_ids(&conn, &posts, "tags", "p1", Some(locale)).unwrap(),
            vec!["t1"],
            "{locale}"
        );
    }

    drop_tables_matching(&conn, &posts);
    drop_tables_matching(&conn, &tags);
}

fn slugged_posts(posts: &str, unique: bool) -> Registry {
    let mut def = CollectionDefinition::new(posts);
    def.fields = vec![
        FieldDefinition::builder("slug", FieldType::Text)
            .unique(unique)
            .build(),
    ];

    registry_of(def)
}

/// Regression: the inline `UNIQUE` an older release put on a unique field
/// survived the upgrade, so removing `unique` still failed every duplicate
/// write at the database. The sync drops it in place, once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_an_older_tables_inline_unique_is_dropped() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("f5posts");

    {
        let conn = pool.get().expect("conn");
        conn.execute_batch(&format!(
            "CREATE TABLE \"{posts}\" (id TEXT PRIMARY KEY, slug TEXT UNIQUE, \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT); \
             INSERT INTO \"{posts}\" (id, slug) VALUES ('p1', 'hello');"
        ))
        .unwrap();
    }

    sync_all(&pool, &slugged_posts(&posts, false), &no_locale()).expect("sync");

    let conn = pool.get().expect("conn");
    assert_eq!(unique_constraints(&conn, &posts), 0);
    conn.execute(
        &format!("INSERT INTO \"{posts}\" (id, slug) VALUES ('p2', 'hello')"),
        &[],
    )
    .expect("a field without `unique` accepts a duplicate");

    drop_tables_matching(&conn, &posts);
}

/// With the field still `unique`, the managed index enforces it once the
/// inline constraint is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_unique_field_keeps_its_uniqueness_through_the_drop() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("f5uposts");

    {
        let conn = pool.get().expect("conn");
        conn.execute_batch(&format!(
            "CREATE TABLE \"{posts}\" (id TEXT PRIMARY KEY, slug TEXT UNIQUE, \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT); \
             INSERT INTO \"{posts}\" (id, slug) VALUES ('p1', 'hello');"
        ))
        .unwrap();
    }

    sync_all(&pool, &slugged_posts(&posts, true), &no_locale()).expect("sync");

    let conn = pool.get().expect("conn");
    assert_eq!(unique_constraints(&conn, &posts), 0);
    conn.execute(
        &format!("INSERT INTO \"{posts}\" (id, slug) VALUES ('p2', 'hello')"),
        &[],
    )
    .expect_err("the managed unique index rejects the duplicate");

    drop_tables_matching(&conn, &posts);
}

/// The `_ref_count` of tag `id`.
fn ref_count(conn: &dyn DbConnection, tags: &str, id: &str) -> i64 {
    conn.query_one(
        &format!("SELECT _ref_count FROM \"{tags}\" WHERE id = $1"),
        &[DbValue::Text(id.to_string())],
    )
    .unwrap()
    .and_then(|row| row.i64_at(0))
    .unwrap()
}

/// Regression: a has-many relationship that stopped being localized kept
/// every locale's junction rows, and its reads — which no longer name a
/// locale — returned them all as one list. The sync keeps the default
/// locale's rows in order, drops the others and recounts the references.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_an_unlocalized_junction_keeps_its_default_locales_rows() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("fu1posts");
    let tags = unique_slug("fu1tags");

    sync_all(&pool, &tagged_posts(&posts, &tags, true), &locale_en_de()).expect("localized");

    {
        let conn = pool.get().expect("conn");
        conn.execute_batch(&format!(
            "INSERT INTO \"{tags}\" (id) VALUES ('t1'), ('t2'), ('t3'); \
             INSERT INTO \"{posts}\" (id) VALUES ('p1'); \
             INSERT INTO \"{posts}_tags\" (parent_id, related_id, _order, _locale) VALUES \
               ('p1', 't1', 0, 'en'), ('p1', 't2', 1, 'en'), \
               ('p1', 't2', 0, 'de'), ('p1', 't3', 1, 'de'); \
             UPDATE \"{tags}\" SET _ref_count = 1 WHERE id IN ('t1', 't3'); \
             UPDATE \"{tags}\" SET _ref_count = 2 WHERE id = 't2';"
        ))
        .unwrap();
    }

    sync_all(&pool, &tagged_posts(&posts, &tags, false), &locale_en_de()).expect("unlocalized");

    let conn = pool.get().expect("conn");
    assert_eq!(
        find_related_ids(&conn, &posts, "tags", "p1", None).unwrap(),
        vec!["t1", "t2"]
    );
    assert_eq!(ref_count(&conn, &tags, "t1"), 1);
    assert_eq!(
        ref_count(&conn, &tags, "t2"),
        1,
        "the `de` reference is gone"
    );
    assert_eq!(ref_count(&conn, &tags, "t3"), 0, "only `de` named it");

    drop_tables_matching(&conn, &posts);
    drop_tables_matching(&conn, &tags);
}
