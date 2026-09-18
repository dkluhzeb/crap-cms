//! Helpers shared by the Postgres harness tests: fixtures, catalog counts
//! and cleanup.

#![cfg(all(test, feature = "postgres"))]

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig,
        VersionsConfig,
    },
    db::{DbConnection, DbValue},
};

/// Localization off — the soft-delete transition is about constraints, not
/// locale columns.
pub(super) fn no_locale() -> LocaleConfig {
    LocaleConfig::default()
}

/// `TEST_DATABASE_URL` with its password replaced, or `None` when the URL
/// carries none (trust authentication cannot reject a password).
pub(super) fn with_wrong_password(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let at = rest.find('@')?;
    let (credentials, host) = rest.split_at(at);
    let user = credentials.split_once(':')?.0;

    Some(format!("{scheme}://{user}:definitely-wrong{host}"))
}

/// A `posts` collection with a unique field and a has-many relationship,
/// versioned — so its table has a junction table and a versions table
/// pointing at it with `ON DELETE CASCADE`.
pub(super) fn soft_delete_registry(posts: &str, tags: &str, soft_delete: bool) -> Registry {
    let mut def = CollectionDefinition::new(posts);
    def.soft_delete = soft_delete;
    def.versions = Some(VersionsConfig::new(true, 10));
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .unique(true)
            .build(),
        FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new(tags, true))
            .build(),
    ];

    let mut registry = Registry::new();
    registry.register_collection(CollectionDefinition::new(tags));
    registry.register_collection(def);

    registry
}

pub(super) fn row_count(conn: &dyn DbConnection, table: &str) -> i64 {
    conn.query_one(&format!("SELECT COUNT(*) AS c FROM \"{table}\""), &[])
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap()
}

/// How many foreign keys on `child` point at `parent`.
pub(super) fn foreign_keys_to(conn: &dyn DbConnection, child: &str, parent: &str) -> i64 {
    catalog_count(
        conn,
        "SELECT COUNT(*) AS c FROM pg_constraint \
         WHERE contype = 'f' AND conrelid = to_regclass($1) AND confrelid = to_regclass($2)",
        &[
            DbValue::Text(format!("\"{child}\"")),
            DbValue::Text(format!("\"{parent}\"")),
        ],
    )
}

pub(super) fn unique_constraints(conn: &dyn DbConnection, table: &str) -> i64 {
    catalog_count(
        conn,
        "SELECT COUNT(*) AS c FROM pg_constraint \
         WHERE contype = 'u' AND conrelid = to_regclass($1)",
        &[DbValue::Text(format!("\"{table}\""))],
    )
}

/// How many named statements this session holds on the server.
pub(super) fn prepared_statement_count(conn: &dyn DbConnection) -> i64 {
    catalog_count(
        conn,
        "SELECT COUNT(*) AS c FROM pg_prepared_statements",
        &[],
    )
}

pub(super) fn catalog_count(conn: &dyn DbConnection, sql: &str, params: &[DbValue]) -> i64 {
    conn.query_one(sql, params)
        .unwrap()
        .unwrap()
        .get_i64("c")
        .unwrap()
}

/// Drop every table whose name carries `slug` — the collection table and
/// everything the sync derived from it (junction, versions, FTS).
pub(super) fn drop_tables_matching(conn: &dyn DbConnection, slug: &str) {
    let tables = conn.list_user_tables().unwrap();

    for table in tables.iter().filter(|t| t.contains(slug)) {
        conn.execute_ddl(&format!("DROP TABLE IF EXISTS \"{table}\" CASCADE"), &[])
            .unwrap();
    }
}
