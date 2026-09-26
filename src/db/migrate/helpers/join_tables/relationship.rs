//! Junction tables for has-many relationship and upload fields.

use anyhow::Result;

use crate::config::LocaleConfig;
use crate::core::FieldDefinition;
use crate::db::DbConnection;
use crate::db::migrate::helpers::introspection::table_exists;
use crate::db::migrate::relationship_target::track_target;
use crate::db::query::helpers::join_table;

use super::junction_shape::{JunctionShape, create_junction_table, reconcile_junction_shape};

/// Sync a has-many relationship junction table: create it, or bring the
/// stored one to the shape the field wants (see [`reconcile_junction_shape`]).
pub(super) fn sync_relationship_table(
    conn: &dyn DbConnection,
    collection_slug: &str,
    field: &FieldDefinition,
    full_name: &str,
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let rc = match &field.relationship {
        Some(rc) if rc.has_many => rc,
        _ => return Ok(()),
    };

    let table_name = join_table(collection_slug, full_name);
    let shape = JunctionShape::new(
        rc.is_polymorphic(),
        has_locale_col.then_some(locale_config.default_locale.as_str()),
    );

    if table_exists(conn, &table_name)? {
        reconcile_junction_shape(conn, &table_name, collection_slug, &shape)?;
    } else {
        create_junction_table(conn, &table_name, collection_slug, &shape)?;
    }

    // The rows hold bare ids; which collection they belong to is the
    // definition's word alone, so the table records the target it was written
    // against and the next sync can say when that word changed.
    track_target(conn, &table_name, rc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldType, RelationshipConfig};
    use crate::db::DbValue;
    use crate::db::migrate::collection::{create_collection_table, test_helpers::*};
    use crate::db::migrate::helpers::introspection::get_table_columns;
    use crate::db::migrate::helpers::join_tables::sync_join_tables;

    #[test]
    fn has_many_relationship_creates_junction_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ],
        );
        // Need parent table first for FK
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("related_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn localized_has_many_creates_junction_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .localized(true)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(
            cols.contains("_locale"),
            "Localized junction table should have _locale column"
        );
    }

    #[test]
    fn existing_has_many_adds_locale_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        // Create parent and junction table without _locale
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
            "CREATE TABLE posts_tags (parent_id TEXT, related_id TEXT, _order INTEGER)",
            &[],
        )
        .unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .localized(true)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("_locale"));
    }

    #[test]
    fn group_relationship_creates_prefixed_junction_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("tags", FieldType::Relationship)
                            .relationship(RelationshipConfig::new("tags", true))
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__tags").unwrap(),
            "Group > Relationship should create prefixed junction table"
        );
        let cols = get_table_columns(&conn, "posts_config__tags").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("related_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn localized_group_has_many_inherits_locale_column() {
        // Regression: has-many relationships inside localized Groups missed _locale column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("tags", FieldType::Relationship)
                            .relationship(RelationshipConfig::new("tags", true))
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_meta__tags").unwrap();
        assert!(
            cols.contains("_locale"),
            "has-many Relationship inside localized Group should inherit _locale column"
        );
    }

    #[test]
    fn polymorphic_upgrade_rebuilds_primary_key() {
        let text = |s: &str| DbValue::Text(s.to_string());
        let int = |n: i64| DbValue::Integer(n);

        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Step 1: Create a non-polymorphic junction table (simulates old schema)
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
            "CREATE TABLE posts_related (\
            parent_id TEXT NOT NULL, \
            related_id TEXT NOT NULL, \
            _order INTEGER NOT NULL DEFAULT 0, \
            PRIMARY KEY (parent_id, related_id)\
        )",
            &[],
        )
        .unwrap();

        // Step 2: Insert parent row and junction data
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, _order) VALUES (?1, ?2, ?3)",
            &[text("p1"), text("r1"), int(0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, _order) VALUES (?1, ?2, ?3)",
            &[text("p1"), text("r2"), int(1)],
        )
        .unwrap();

        // Step 3: Run the upgrade (simulating schema change to polymorphic)
        let mut rc = RelationshipConfig::new("tags", true);
        rc.polymorphic = vec!["tags".into(), "categories".into()];

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("related", FieldType::Relationship)
                    .relationship(rc)
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        // Step 4: Verify data is preserved
        let rows = conn
        .query_all("SELECT parent_id, related_id, related_collection, _order FROM posts_related ORDER BY _order", &[])
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_string("parent_id").unwrap(), "p1");
        assert_eq!(rows[0].get_string("related_id").unwrap(), "r1");
        assert_eq!(rows[0].get_string("related_collection").unwrap(), "");
        assert_eq!(rows[1].get_string("related_id").unwrap(), "r2");

        // Step 5: Verify the new PK allows duplicate (parent_id, related_id)
        // with different related_collection values
        conn.execute(
        "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES (?1, ?2, ?3, ?4)",
        &[text("p1"), text("r1"), text("categories"), int(2)],
    )
    .unwrap();

        let count = conn
            .query_all(
                "SELECT * FROM posts_related WHERE parent_id = ?1 AND related_id = ?2",
                &[text("p1"), text("r1")],
            )
            .unwrap();
        assert_eq!(
            count.len(),
            2,
            "Same (parent_id, related_id) with different related_collection should be allowed"
        );
    }

    #[test]
    fn polymorphic_upgrade_with_locale_preserves_data() {
        let text = |s: &str| DbValue::Text(s.to_string());
        let int = |n: i64| DbValue::Integer(n);

        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Create a non-polymorphic localized junction table
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
            "CREATE TABLE posts_related (\
            parent_id TEXT NOT NULL, \
            related_id TEXT NOT NULL, \
            _order INTEGER NOT NULL DEFAULT 0, \
            _locale TEXT NOT NULL DEFAULT 'en', \
            PRIMARY KEY (parent_id, related_id, _locale)\
        )",
            &[],
        )
        .unwrap();

        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
        "INSERT INTO posts_related (parent_id, related_id, _order, _locale) VALUES (?1, ?2, ?3, ?4)",
        &[text("p1"), text("r1"), int(0), text("en")],
    )
    .unwrap();
        conn.execute(
        "INSERT INTO posts_related (parent_id, related_id, _order, _locale) VALUES (?1, ?2, ?3, ?4)",
        &[text("p1"), text("r1"), int(0), text("de")],
    )
    .unwrap();

        // Upgrade to polymorphic
        let mut rc = RelationshipConfig::new("tags", true);
        rc.polymorphic = vec!["tags".into(), "categories".into()];

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("related", FieldType::Relationship)
                    .localized(true)
                    .relationship(rc)
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        // Data preserved
        let rows = conn
            .query_all("SELECT * FROM posts_related ORDER BY _locale", &[])
            .unwrap();
        assert_eq!(rows.len(), 2);

        // related_collection column exists
        let cols = get_table_columns(&conn, "posts_related").unwrap();
        assert!(cols.contains("related_collection"));
        assert!(cols.contains("_locale"));

        // Regression: the rebuilt `_locale` column lost its default, so a row
        // written without a locale was stored with none.
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) \
             VALUES ('p1', 'r2', 'tags', 1)",
            &[],
        )
        .unwrap();
        let row = conn
            .query_one(
                "SELECT _locale FROM posts_related WHERE related_id = 'r2'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("_locale").unwrap(), "en");
    }

    #[test]
    fn polymorphic_junction_rebuild_preserves_fk() {
        // Regression: the polymorphic junction rebuild dropped the
        // REFERENCES ... ON DELETE CASCADE constraint on parent_id.
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Create parent table
        conn.execute(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, _ref_count INTEGER DEFAULT 0)",
            &[],
        )
        .unwrap();

        // Create a non-polymorphic junction table with FK
        conn.execute_batch(
            "CREATE TABLE posts_tags (\
            parent_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE, \
            related_id TEXT NOT NULL, \
            _order INTEGER NOT NULL DEFAULT 0, \
            PRIMARY KEY (parent_id, related_id)\
        )",
        )
        .unwrap();

        // Insert test data
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 'tag1', 0)",
            &[],
        )
        .unwrap();

        // Rebuild for polymorphic upgrade
        reconcile_junction_shape(
            &conn,
            "posts_tags",
            "posts",
            &JunctionShape::new(true, None),
        )
        .unwrap();

        // Verify columns
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(
            cols.contains("related_collection"),
            "must have related_collection"
        );

        // Verify data migrated
        let rows = conn
            .query_all("SELECT parent_id, related_id FROM posts_tags", &[])
            .unwrap();
        assert_eq!(rows.len(), 1);

        // Verify FK still works: cascade delete should remove junction row
        conn.execute("DELETE FROM posts WHERE id = 'p1'", &[])
            .unwrap();
        let rows = conn.query_all("SELECT * FROM posts_tags", &[]).unwrap();
        assert_eq!(
            rows.len(),
            0,
            "FK ON DELETE CASCADE must be preserved after polymorphic rebuild"
        );
    }
}
