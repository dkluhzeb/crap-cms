//! What a junction table's stored ids point at.
//!
//! A junction row holds a bare id; which collection that id belongs to comes
//! from the field definition alone. Re-pointing a relationship at another
//! collection therefore changes the meaning of every stored row without
//! touching one of them — the ids stay, and resolve against nothing. The
//! target each table was written against is recorded in `_crap_meta` so the
//! next boot can say so.

use anyhow::Result;
use tracing::warn;

use crate::{
    core::RelationshipConfig,
    db::{DbConnection, migrate::meta, query::helpers::quote_ident},
};

/// The `_crap_meta` key recording the collection(s) a junction table's rows
/// were written against.
fn meta_key(table: &str) -> String {
    format!("relationship_target:{table}")
}

/// The target a relationship writes its ids against: the single collection, or
/// every polymorphic target, sorted so a reordered list is not a retarget.
fn target_of(rc: &RelationshipConfig) -> String {
    if rc.is_polymorphic() {
        let mut targets: Vec<String> = rc.polymorphic.iter().map(ToString::to_string).collect();
        targets.sort();

        return targets.join(",");
    }

    rc.collection.to_string()
}

/// How many rows a junction table holds.
fn row_count(conn: &dyn DbConnection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) AS cnt FROM {}", quote_ident(table));

    Ok(conn
        .query_one(&sql, &[])?
        .and_then(|row| row.get_i64("cnt").ok())
        .unwrap_or(0))
}

/// Warn when a re-pointed table still holds rows: those ids were written
/// against the previous target and resolve against nothing in the new one.
fn warn_repointed(
    conn: &dyn DbConnection,
    table: &str,
    previous: &str,
    target: &str,
) -> Result<()> {
    let rows = row_count(conn, table)?;

    if rows == 0 {
        return Ok(());
    }

    warn!(
        "Relationship table '{table}' now points at '{target}' but its {rows} row(s) were \
         written against '{previous}' — those ids resolve against nothing in '{target}'. \
         Re-point the relationship back, or clear the table's rows."
    );

    Ok(())
}

/// Record the collection a junction table's rows point at, warning first when
/// the definition moved it somewhere else under stored rows.
///
/// # Errors
///
/// Returns a backend error if the meta read/write or the row count fails.
pub(in crate::db::migrate) fn track_target(
    conn: &dyn DbConnection,
    table: &str,
    rc: &RelationshipConfig,
) -> Result<()> {
    let target = target_of(rc);
    let key = meta_key(table);
    let stored = meta::get(conn, &key)?;

    if stored.as_deref() == Some(target.as_str()) {
        return Ok(());
    }

    if let Some(previous) = stored {
        warn_repointed(conn, table, &previous, &target)?;
    }

    meta::upsert(conn, &key, &target)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::{CollectionDefinition, FieldDefinition, FieldType};
    use crate::db::migrate::collection::{create_collection_table, test_helpers::*};
    use crate::db::migrate::helpers::sync_join_tables;

    fn posts_with_target(target: &str) -> CollectionDefinition {
        simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("links", FieldType::Relationship)
                    .relationship(RelationshipConfig::new(target, true))
                    .build(),
            ],
        )
    }

    /// The recorded target follows the definition, so a later sync compares
    /// against what the rows were actually written for.
    #[test]
    fn the_target_is_recorded_on_the_first_sync() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = posts_with_target("tags");
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let recorded = meta::get(&conn, &meta_key("posts_links")).unwrap();
        assert_eq!(recorded.as_deref(), Some("tags"));
    }

    /// Re-pointing the relationship rewrites the record — and the rows that
    /// were written against the old target stay exactly where they were, which
    /// is what the warning is about.
    #[test]
    fn re_pointing_the_relationship_rewrites_the_record() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = posts_with_target("tags");
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        conn.execute_batch(
            "INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_links (parent_id, related_id, _order) VALUES ('p1', 't1', 0);",
        )
        .unwrap();
        assert_eq!(row_count(&conn, "posts_links").unwrap(), 1);

        let moved = posts_with_target("authors");
        sync_join_tables(&conn, "posts", &moved.fields, &no_locale()).unwrap();

        let recorded = meta::get(&conn, &meta_key("posts_links")).unwrap();
        assert_eq!(
            recorded.as_deref(),
            Some("authors"),
            "the record must follow the definition"
        );
        assert_eq!(
            row_count(&conn, "posts_links").unwrap(),
            1,
            "the rows are never touched"
        );
    }

    /// A polymorphic relationship records every target, so adding or removing
    /// one is a change like any other.
    #[test]
    fn a_polymorphic_relationship_records_every_target() {
        let mut rc = RelationshipConfig::new("posts", true);
        rc.polymorphic = vec!["posts".into(), "pages".into()];

        assert_eq!(target_of(&rc), "pages,posts");
    }
}
