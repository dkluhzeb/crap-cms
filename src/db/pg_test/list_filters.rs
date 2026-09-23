//! Postgres harness: element-wise filters on scalar has-many lists, has-many
//! relationship junctions and reference lists inside rows — and the schema
//! sync that keeps every stored list a list.

#![cfg(all(test, feature = "postgres"))]

use super::{
    pg_test_pool,
    support::{drop_tables_matching, no_locale},
    unique_slug,
};
use crate::{
    core::{
        BlockDefinition, CollectionDefinition, FieldDefinition, FieldType, Registry,
        RelationshipConfig,
    },
    db::{
        DbConnection, DbPool, DbValue, Filter, FilterClause, FilterOp, migrate::sync_all,
        query::filter::build_where_clause,
    },
};

/// `(id, tags, scores, block data)`: a Text list, a Number list, and a block
/// row holding a Text list and a polymorphic reference list. The same ids the
/// `tags` lists hold are the `refs` junction's, the `items` row's `related`
/// list's and — as `things/…` entries — the block's `links`.
const ROWS: [(&str, Option<&str>, Option<&str>, &str); 4] = [
    (
        "a",
        Some(r#"["a","b"]"#),
        Some("[1,2.5]"),
        r#"{"tags":["a","b"],"links":["things/a","things/b"]}"#,
    ),
    (
        "b",
        Some(r#"["c"]"#),
        Some("[10]"),
        r#"{"tags":["c"],"links":["things/c"]}"#,
    ),
    ("empty", Some("[]"), Some("[]"), r#"{"tags":[],"links":[]}"#),
    ("null", None, None, "{}"),
];

fn references(name: &str, collections: &[&str]) -> FieldDefinition {
    let mut config = RelationshipConfig::new(collections[0], true);

    if collections.len() > 1 {
        config.polymorphic = collections.iter().map(|c| (*c).into()).collect();
    }

    FieldDefinition::builder(name, FieldType::Relationship)
        .relationship(config)
        .build()
}

fn list_fields() -> Vec<FieldDefinition> {
    let list =
        |name: &str, ft: FieldType| FieldDefinition::builder(name, ft).has_many(true).build();

    vec![
        list("tags", FieldType::Text),
        list("scores", FieldType::Number),
        references("refs", &["things"]),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![references("related", &["things"])])
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "card",
                vec![
                    list("tags", FieldType::Text),
                    references("links", &["things", "others"]),
                ],
            )])
            .build(),
    ]
}

/// The parent table holding [`ROWS`], the `refs` junction holding the ids
/// the `tags` lists hold, and an array and a blocks table with one row per
/// document.
fn seed(conn: &dyn DbConnection, slug: &str) {
    conn.execute_ddl(
        &format!("CREATE TABLE \"{slug}\" (id TEXT PRIMARY KEY, tags TEXT, scores TEXT)"),
        &[],
    )
    .unwrap();
    conn.execute_ddl(
        &format!("CREATE TABLE \"{slug}_refs\" (parent_id TEXT, related_id TEXT)"),
        &[],
    )
    .unwrap();
    conn.execute_ddl(
        &format!("CREATE TABLE \"{slug}_items\" (parent_id TEXT, related TEXT)"),
        &[],
    )
    .unwrap();
    conn.execute_ddl(
        &format!("CREATE TABLE \"{slug}_content\" (parent_id TEXT, _block_type TEXT, data TEXT)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!(
            "INSERT INTO \"{slug}_refs\" (parent_id, related_id) \
             VALUES ('a', 'a'), ('a', 'b'), ('b', 'c')"
        ),
        &[],
    )
    .unwrap();

    for (id, tags, scores, block) in ROWS {
        insert_row(conn, slug, id, tags, scores, block);
    }
}

fn insert_row(
    conn: &dyn DbConnection,
    slug: &str,
    id: &str,
    tags: Option<&str>,
    scores: Option<&str>,
    block: &str,
) {
    let text = |v: Option<&str>| v.map_or(DbValue::Null, |v| DbValue::Text(v.into()));

    conn.execute(
        &format!("INSERT INTO \"{slug}\" (id, tags, scores) VALUES ($1, $2, $3)"),
        &[DbValue::Text(id.into()), text(tags), text(scores)],
    )
    .unwrap();
    conn.execute(
        &format!("INSERT INTO \"{slug}_items\" (parent_id, related) VALUES ($1, $2)"),
        &[DbValue::Text(id.into()), text(tags)],
    )
    .unwrap();
    conn.execute(
        &format!(
            "INSERT INTO \"{slug}_content\" (parent_id, _block_type, data) VALUES ($1, 'card', $2)"
        ),
        &[DbValue::Text(id.into()), DbValue::Text(block.into())],
    )
    .unwrap();
}

/// The ids Postgres matches for `field op`, sorted.
fn ids_for(conn: &dyn DbConnection, slug: &str, field: &str, op: &FilterOp) -> Vec<String> {
    let filters = vec![FilterClause::Single(Filter {
        field: field.to_string(),
        op: op.clone(),
    })];
    let mut params = Vec::new();
    let clause = build_where_clause(conn, &filters, slug, &list_fields(), None, &mut params)
        .unwrap_or_else(|e| panic!("{field} {op:?}: {e:#}"));

    conn.query_all(
        &format!("SELECT id FROM \"{slug}\"{clause} ORDER BY id"),
        &params,
    )
    .unwrap_or_else(|e| panic!("{field} {op:?} must execute on Postgres: {e:#}"))
    .iter()
    .filter_map(|row| row.opt_text_at(0))
    .collect()
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_string()).collect()
}

fn text_cases() -> Vec<(FilterOp, Vec<String>)> {
    vec![
        (FilterOp::Equals("a".into()), ids(&["a"])),
        (FilterOp::Equals("z".into()), ids(&[])),
        (
            FilterOp::NotEquals("a".into()),
            ids(&["b", "empty", "null"]),
        ),
        (FilterOp::In(vec!["a".into(), "c".into()]), ids(&["a", "b"])),
        (
            FilterOp::NotIn(vec!["a".into(), "c".into()]),
            ids(&["empty", "null"]),
        ),
        (FilterOp::Like("B".into()), ids(&["a"])),
        (FilterOp::Contains("c".into()), ids(&["b"])),
        (FilterOp::Exists, ids(&["a", "b"])),
        (FilterOp::NotExists, ids(&["empty", "null"])),
    ]
}

fn number_cases() -> Vec<(FilterOp, Vec<String>)> {
    vec![
        (FilterOp::Equals("2.5".into()), ids(&["a"])),
        (
            FilterOp::NotEquals("10".into()),
            ids(&["a", "empty", "null"]),
        ),
        // Numeric, not lexical: "10" sorts before "9" as text.
        (FilterOp::GreaterThan("9".into()), ids(&["b"])),
        (FilterOp::LessThan("2".into()), ids(&["a"])),
        (FilterOp::GreaterThanOrEqual("2.5".into()), ids(&["a", "b"])),
        (FilterOp::LessThanOrEqual("1".into()), ids(&["a"])),
        (
            FilterOp::In(vec!["1".into(), "10".into()]),
            ids(&["a", "b"]),
        ),
        (
            FilterOp::NotIn(vec!["1".into(), "10".into()]),
            ids(&["empty", "null"]),
        ),
        (FilterOp::Contains("5".into()), ids(&["a"])),
        (FilterOp::Exists, ids(&["a", "b"])),
        (FilterOp::NotExists, ids(&["empty", "null"])),
    ]
}

/// Postgres runs every list operator element by element — on a Text list, a
/// Number list (compared numerically), a list inside a block row, a has-many
/// relationship junction, and a reference list in an array row and in a block
/// (polymorphic, compared by the id after its collection).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_list_filters_match_element_by_element() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let slug = unique_slug("listfilter");
    seed(&conn, &slug);

    for (op, expected) in text_cases() {
        assert_eq!(ids_for(&conn, &slug, "tags", &op), expected, "tags {op:?}");
        for field in ["refs.id", "items.related", "content.links"] {
            assert_eq!(
                ids_for(&conn, &slug, field, &op),
                expected,
                "{field} {op:?}"
            );
        }
    }

    for (op, expected) in number_cases() {
        assert_eq!(
            ids_for(&conn, &slug, "scores", &op),
            expected,
            "scores {op:?}"
        );
    }

    let block_hit = ids_for(&conn, &slug, "content.tags", &FilterOp::Equals("b".into()));
    assert_eq!(block_hit, ids(&["a"]), "content.tags equals b");

    let block_none = ids_for(
        &conn,
        &slug,
        "content.tags",
        &FilterOp::NotIn(vec!["a".into()]),
    );
    assert_eq!(
        block_none,
        ids(&["b", "empty", "null"]),
        "content.tags not_in [a]: some row whose list holds no a"
    );

    drop_tables_matching(&conn, &slug);
}

// ── The schema sync keeps every stored list a list ────────────────────────

/// A collection whose `tags` (text), `scores` (number) and `code` (text)
/// fields hold lists when `has_many`, and whose `items` rows hold a
/// relationship — a has-many one when `has_many`.
fn sync_registry(slug: &str, has_many: bool, code_type: FieldType) -> Registry {
    let field = |name: &str, ft: FieldType| {
        FieldDefinition::builder(name, ft)
            .has_many(has_many)
            .build()
    };
    let related = FieldDefinition::builder("related", FieldType::Relationship)
        .relationship(RelationshipConfig::new("things", has_many))
        .build();

    let mut def = CollectionDefinition::new(slug);
    def.fields = vec![
        field("tags", FieldType::Text),
        field("scores", FieldType::Number),
        field("code", code_type),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![related])
            .build(),
    ];

    let mut registry = Registry::new();
    registry.register_collection(def);

    registry
}

/// Create the collection with single-value fields and store one document.
fn seed_single_values(pool: &DbPool, slug: &str) {
    sync_all(
        pool,
        &sync_registry(slug, false, FieldType::Text),
        &no_locale(),
    )
    .expect("initial sync");

    let conn = pool.get().expect("conn");
    conn.execute(
        &format!(
            "INSERT INTO \"{slug}\" (id, tags, scores, code) VALUES ('p1', 'news', 5, 'abc'), \
             ('p2', '', 7.5, NULL)"
        ),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!(
            "INSERT INTO \"{slug}_items\" (id, parent_id, _order, related) \
             VALUES ('i1', 'p1', 0, 't1')"
        ),
        &[],
    )
    .unwrap();
}

fn stored(conn: &dyn DbConnection, sql: &str) -> Option<String> {
    conn.query_one(sql, &[])
        .unwrap()
        .and_then(|row| row.opt_text_at(0))
}

/// Switching fields to `has_many` over stored single values stores each as a
/// one-element list — a number column reconciled to text included, blank text
/// as NULL — so the list filters read them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_sync_stores_single_values_as_lists() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let slug = unique_slug("hmsync");
    seed_single_values(&pool, &slug);

    sync_all(
        &pool,
        &sync_registry(&slug, true, FieldType::Text),
        &no_locale(),
    )
    .expect("sync switching the fields to has_many");

    let conn = pool.get().expect("conn");
    let at = |sql: String| stored(&conn, &sql);

    assert_eq!(
        at(format!("SELECT tags FROM \"{slug}\" WHERE id = 'p1'")).as_deref(),
        Some(r#"["news"]"#)
    );
    assert_eq!(
        at(format!("SELECT scores FROM \"{slug}\" WHERE id = 'p1'")).as_deref(),
        Some("[5]")
    );
    assert_eq!(
        at(format!("SELECT tags FROM \"{slug}\" WHERE id = 'p2'")),
        None
    );
    assert_eq!(
        at(format!("SELECT scores FROM \"{slug}\" WHERE id = 'p2'")).as_deref(),
        Some("[7.5]")
    );
    assert_eq!(
        at(format!("SELECT related FROM \"{slug}_items\"")).as_deref(),
        Some(r#"["t1"]"#)
    );

    let registry = sync_registry(&slug, true, FieldType::Text);
    let def = registry
        .collections
        .values()
        .next()
        .expect("the collection");
    let filters = vec![FilterClause::Single(Filter {
        field: "scores".to_string(),
        op: FilterOp::GreaterThan("6".into()),
    })];
    let mut params = Vec::new();
    let clause =
        build_where_clause(&conn, &filters, &slug, &def.fields, None, &mut params).unwrap();
    let rows = conn
        .query_all(&format!("SELECT id FROM \"{slug}\"{clause}"), &params)
        .expect("a number list filter runs over the stored lists");
    assert_eq!(rows.len(), 1);

    drop_tables_matching(&conn, &slug);
}

/// Text in a field switched to a number list can't become a number: the sync
/// fails naming the column and the document, and rolls back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_sync_refuses_text_in_a_number_list() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let slug = unique_slug("hmrefuse");
    seed_single_values(&pool, &slug);

    let err = sync_all(
        &pool,
        &sync_registry(&slug, true, FieldType::Number),
        &no_locale(),
    )
    .expect_err("text in a number list must fail the sync");
    let message = format!("{err:#}");

    assert!(
        message.contains(&format!("{slug}.code, document 'p1': holds \"abc\"")),
        "{message}"
    );

    let conn = pool.get().expect("conn");
    assert_eq!(
        stored(
            &conn,
            &format!("SELECT tags FROM \"{slug}\" WHERE id = 'p1'")
        )
        .as_deref(),
        Some("news"),
        "the failed sync rolls back"
    );

    drop_tables_matching(&conn, &slug);
}
