//! **One-time conversion — removable after 0.1.0** (see [`super::one_time`]).
//!
//! Stores the values of existing JSON-stored rows — blocks rows, and the groups
//! and arrays nested in an array row — in their typed form: a checkbox as
//! `true`/`false`, a number as a number, a timezone date as UTC (the rule is
//! [`nested_value`](crate::db::query::helpers::nested_value)). Writes store that
//! form; this converts the rows written before, once per collection or global.
//! Storing a stored value again changes nothing, so an interrupted run simply
//! continues.

use std::slice;

use anyhow::{Context as _, Result};
use serde_json::{Map, Value};
use tracing::info;

use crate::{
    core::{
        BlockDefinition, FieldChildren, FieldDefinition, Registry, field_children,
        flatten_array_sub_fields,
    },
    db::{
        DbConnection, DbValue,
        migrate::helpers::{
            Scan, block_paths, field_paths, for_each_row, update_by_id, versioned_fingerprint,
        },
        query::{
            helpers::{global_table, join_table, prefixed_name},
            join::store_nested_values,
        },
    },
};

use super::meta;

/// Leads the meta value; bump to force a re-run after a change here. The rest
/// of the value fingerprints the join-table columns a pass covered, so a
/// sub-field added or retyped later — including one added to a collection
/// whose join table already existed — runs it again.
const MIGRATION_VERSION: &str = "2";

/// The gate of one target, keyed by its table so a collection and a global of
/// the same slug can't share one — sharing it would leave the two rewriting
/// each other's gate on every boot.
fn meta_key(table: &str) -> String {
    format!("nested_values:{table}")
}

/// The meta key of the conversion this one replaced — timezone dates only —
/// whose gate is removed.
fn replaced_meta_key(slug: &str) -> String {
    format!("nested_timezone_dates:{slug}")
}

/// A join-table field of a collection or global, with its storage key
/// (`group__field` when nested in a group).
enum JoinField<'a> {
    Array {
        key: String,
        sub: &'a [FieldDefinition],
    },
    Blocks {
        key: String,
        defs: &'a [BlockDefinition],
    },
}

/// Store the nested values of every collection and global not yet at the
/// current version.
///
/// # Errors
///
/// Returns a backend error if a SELECT, an UPDATE, or the meta upsert fails.
pub(super) fn convert_if_needed(conn: &dyn DbConnection, registry: &Registry) -> Result<()> {
    for (slug, def) in &registry.collections {
        convert_one(conn, slug, slug, &def.fields)?;
    }

    for (slug, def) in &registry.globals {
        convert_one(conn, slug, &global_table(slug), &def.fields)?;
    }

    Ok(())
}

fn convert_one(
    conn: &dyn DbConnection,
    slug: &str,
    table: &str,
    fields: &[FieldDefinition],
) -> Result<()> {
    // The replaced conversion's gate would outlive it; deleting an absent key
    // changes nothing, so this runs whether or not this conversion has.
    meta::delete(conn, &replaced_meta_key(slug))?;

    // This conversion's own gate was once keyed by slug. A collection's slug IS
    // its table, so only a global left a row behind — one that names no pass any
    // more and would sit in the database forever.
    if table != slug {
        meta::delete(conn, &meta_key(slug))?;
    }

    let mut join_fields = Vec::new();
    collect_join_fields(fields, "", &mut join_fields);
    if join_fields.is_empty() {
        return Ok(());
    }

    let key = meta_key(table);
    let gate = gate_value(table, &join_fields);
    if meta::get(conn, &key)?.as_deref() == Some(gate.as_str()) {
        return Ok(());
    }

    let converted = convert_join_tables(conn, table, &join_fields)?;

    if converted > 0 {
        info!("Stored the nested values of {converted} row(s) of '{slug}' in their typed form");
    }

    meta::upsert(conn, &key, &gate)
}

/// Every leaf takes part in the fingerprint: a blank value nests as null
/// whatever its type, so any sub-field added or retyped can change what a row
/// of the table stores.
fn keeps_every_leaf(_: &FieldDefinition) -> bool {
    true
}

/// `{version}:{fingerprint}` of the join-table columns a pass covers.
fn gate_value(table: &str, join_fields: &[JoinField<'_>]) -> String {
    let mut parts = Vec::new();

    for field in join_fields {
        match field {
            JoinField::Array { key, sub } => {
                let join = join_table(table, key);

                for sf in json_sub_fields(sub) {
                    let paths = field_paths(slice::from_ref(sf), &keeps_every_leaf);
                    parts.push(format!("{join}.{}={paths}", sf.name));
                }
            }
            JoinField::Blocks { key, defs } => {
                let join = join_table(table, key);
                parts.push(format!(
                    "{join}.data={}",
                    block_paths(defs, &keeps_every_leaf)
                ));
            }
        }
    }

    versioned_fingerprint(MIGRATION_VERSION, &parts)
}

/// Convert every join table of `join_fields`, returning the number of rows
/// changed.
fn convert_join_tables(
    conn: &dyn DbConnection,
    table: &str,
    join_fields: &[JoinField<'_>],
) -> Result<usize> {
    let mut converted = 0;

    for field in join_fields {
        converted += match field {
            JoinField::Array { key, sub } => {
                convert_array_table(conn, &join_table(table, key), sub)?
            }
            JoinField::Blocks { key, defs } => {
                convert_blocks_table(conn, &join_table(table, key), defs)?
            }
        };
    }

    Ok(converted)
}

/// Collect the array and blocks fields that own a join table, keyed the way
/// the join writer keys them: a group extends the prefix, layout wrappers and
/// tabs are transparent.
fn collect_join_fields<'a>(
    fields: &'a [FieldDefinition],
    prefix: &str,
    out: &mut Vec<JoinField<'a>>,
) {
    for field in fields {
        let key = prefixed_name(prefix, &field.name);

        match field_children(field) {
            FieldChildren::Array(sub) => out.push(JoinField::Array { key, sub }),
            FieldChildren::Blocks(defs) => out.push(JoinField::Blocks { key, defs }),
            FieldChildren::Group(sub) => collect_join_fields(sub, &key, out),
            FieldChildren::Wrapper(sub) => collect_join_fields(sub, prefix, out),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_join_fields(&tab.fields, prefix, out);
                }
            }
            FieldChildren::Leaf => {}
        }
    }
}

/// Convert the JSON-stored sub-values (groups, nested arrays, blocks) of every
/// row of an array join table. Direct date columns were always converted.
fn convert_array_table(
    conn: &dyn DbConnection,
    table: &str,
    sub: &[FieldDefinition],
) -> Result<usize> {
    let mut converted = 0;

    for sf in json_sub_fields(sub) {
        converted += convert_array_column(conn, table, sf)?;
    }

    Ok(converted)
}

/// The sub-fields of an array row stored as JSON in their own column: groups,
/// nested arrays and blocks.
fn json_sub_fields(sub: &[FieldDefinition]) -> Vec<&FieldDefinition> {
    flatten_array_sub_fields(sub)
        .into_iter()
        .filter(|sf| {
            matches!(
                field_children(sf),
                FieldChildren::Group(_) | FieldChildren::Array(_) | FieldChildren::Blocks(_)
            )
        })
        .collect()
}

/// Convert the JSON in the column of `sf` for every row of an array join table.
fn convert_array_column(
    conn: &dyn DbConnection,
    table: &str,
    sf: &FieldDefinition,
) -> Result<usize> {
    let column = sf.name.as_str();
    let update = update_by_id(conn, table, column);
    let columns = [column];
    let scan = Scan::builder(table, &columns).build();
    let mut converted = 0;

    for_each_row(conn, &scan, &mut |row| {
        let (Some(id), Some(raw)) = (row.opt_text_at(0), row.opt_text_at(1)) else {
            return Ok(());
        };
        let Some(stored) = stored_column_json(sf, &raw) else {
            return Ok(());
        };

        conn.execute(&update, &[DbValue::Text(stored), DbValue::Text(id)])
            .with_context(|| format!("Failed to convert nested values in {table}.{column}"))?;
        converted += 1;

        Ok(())
    })?;

    Ok(converted)
}

/// The JSON text `raw` — the column of `sf` — stores, or `None` when it isn't
/// JSON or is stored already.
fn stored_column_json(sf: &FieldDefinition, raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;

    let mut holder = Map::new();
    holder.insert(sf.name.clone(), value);
    let before = holder.clone();

    store_nested_values(&mut holder, slice::from_ref(sf));

    (holder != before).then(|| holder[&sf.name].to_string())
}

/// Convert the `data` JSON of every row of a blocks join table, each with its
/// own block definition. Rows of a block type no longer defined are left alone.
fn convert_blocks_table(
    conn: &dyn DbConnection,
    table: &str,
    defs: &[BlockDefinition],
) -> Result<usize> {
    let update = update_by_id(conn, table, "data");
    let columns = ["_block_type", "data"];
    let scan = Scan::builder(table, &columns).build();
    let mut converted = 0;

    for_each_row(conn, &scan, &mut |row| {
        let (Some(id), Some(block_type), Some(raw)) =
            (row.opt_text_at(0), row.opt_text_at(1), row.opt_text_at(2))
        else {
            return Ok(());
        };
        let Some(stored) = stored_block_json(defs, &block_type, &raw) else {
            return Ok(());
        };

        conn.execute(&update, &[DbValue::Text(stored), DbValue::Text(id)])
            .with_context(|| format!("Failed to convert nested values in {table}"))?;
        converted += 1;

        Ok(())
    })?;

    Ok(converted)
}

/// The `data` JSON text a blocks row of `block_type` holding `raw` stores, or
/// `None` when the block type is no longer defined, `raw` isn't a JSON object,
/// or it is stored already.
fn stored_block_json(defs: &[BlockDefinition], block_type: &str, raw: &str) -> Option<String> {
    let def = defs.iter().find(|d| d.block_type == block_type)?;
    let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(raw) else {
        return None;
    };
    let before = data.clone();

    store_nested_values(&mut data, &def.fields);

    (data != before).then(|| Value::Object(data).to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{CollectionDefinition, FieldType, collection::GlobalDefinition},
        db::{InMemoryConn, migrate::helpers::PAGE_SIZE},
    };

    /// Berlin is UTC+1 in January.
    const LOCAL: &str = "2024-01-15T09:00";
    const UTC: &str = "2024-01-15T08:00:00.000Z";

    fn starts() -> FieldDefinition {
        FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build()
    }

    fn registry_with(def: CollectionDefinition) -> Registry {
        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def);

        (*Registry::snapshot(&shared)).clone()
    }

    fn stored_starts(conn: &InMemoryConn, sql: &str) -> Value {
        stored_json(conn, sql)["starts"].clone()
    }

    fn stored_json(conn: &InMemoryConn, sql: &str) -> Value {
        let raw: String = conn.0.query_row(sql, [], |r| r.get(0)).unwrap();

        serde_json::from_str::<Value>(&raw).unwrap()
    }

    /// A connection with the meta table and the given tables.
    fn conn_with(ddl: &str) -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch("CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        conn.0.execute_batch(ddl).unwrap();

        conn
    }

    /// An array field whose row holds a `meta` group with a timezone date and
    /// a checkbox.
    fn items_with_meta() -> FieldDefinition {
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![
                        starts(),
                        FieldDefinition::builder("featured", FieldType::Checkbox).build(),
                    ])
                    .build(),
            ])
            .build()
    }

    fn local_meta() -> String {
        json!({ "starts": LOCAL, "starts_tz": "Europe/Berlin", "featured": "on" }).to_string()
    }

    /// A checkbox inside a group of an array row is stored as `true`/`false`.
    #[test]
    fn converts_a_checkbox_in_an_array_row() {
        let conn = conn_with(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, meta TEXT);",
        );
        conn.0
            .execute(
                "INSERT INTO posts_items VALUES ('r1', 'p1', 0, ?1)",
                [&local_meta()],
            )
            .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![items_with_meta()];
        convert_if_needed(&conn, &registry_with(def)).unwrap();

        let meta = stored_json(&conn, "SELECT meta FROM posts_items WHERE id = 'r1'");
        assert_eq!(meta["featured"], json!(true));
        assert_eq!(meta["starts"], UTC);
    }

    /// A nested array and blocks inside an array row are converted row by row.
    #[test]
    fn converts_arrays_and_blocks_nested_in_an_array_row() {
        let conn = conn_with(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, slots TEXT, content TEXT);",
        );
        let slots = json!([{ "starts": LOCAL, "starts_tz": "Europe/Berlin" }]).to_string();
        let content = json!([
            { "_block_type": "event", "starts": LOCAL, "starts_tz": "Europe/Berlin" }
        ])
        .to_string();
        conn.0
            .execute(
                "INSERT INTO posts_items VALUES ('r1', 'p1', 0, ?1, ?2)",
                [&slots, &content],
            )
            .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("slots", FieldType::Array)
                        .fields(vec![starts()])
                        .build(),
                    FieldDefinition::builder("content", FieldType::Blocks)
                        .blocks(vec![BlockDefinition::new("event", vec![starts()])])
                        .build(),
                ])
                .build(),
        ];
        convert_if_needed(&conn, &registry_with(def)).unwrap();

        assert_eq!(
            stored_json(&conn, "SELECT slots FROM posts_items WHERE id = 'r1'")[0]["starts"],
            UTC
        );
        assert_eq!(
            stored_json(&conn, "SELECT content FROM posts_items WHERE id = 'r1'")[0]["starts"],
            UTC
        );
    }

    /// A global's join table (`_global_{slug}_{field}`) is converted.
    #[test]
    fn converts_a_global_join_table() {
        let conn = conn_with(
            "CREATE TABLE _global_site (id TEXT PRIMARY KEY);
             CREATE TABLE _global_site_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, meta TEXT);",
        );
        conn.0
            .execute(
                "INSERT INTO _global_site_items VALUES ('r1', 'default', 0, ?1)",
                [&local_meta()],
            )
            .unwrap();

        let mut def = GlobalDefinition::new("site");
        def.fields = vec![items_with_meta()];
        let shared = Registry::shared();
        shared.write().unwrap().register_global(def);
        let registry = (*Registry::snapshot(&shared)).clone();

        convert_if_needed(&conn, &registry).unwrap();

        assert_eq!(
            stored_starts(&conn, "SELECT meta FROM _global_site_items WHERE id = 'r1'"),
            UTC
        );
    }

    /// An array inside a group owns a join table keyed with the group prefix
    /// (`posts_grp__items`), which is converted.
    #[test]
    fn converts_a_group_prefixed_join_table() {
        let conn = conn_with(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_grp__items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, meta TEXT);",
        );
        conn.0
            .execute(
                "INSERT INTO posts_grp__items VALUES ('r1', 'p1', 0, ?1)",
                [&local_meta()],
            )
            .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("grp", FieldType::Group)
                .fields(vec![items_with_meta()])
                .build(),
        ];
        convert_if_needed(&conn, &registry_with(def)).unwrap();

        assert_eq!(
            stored_starts(&conn, "SELECT meta FROM posts_grp__items WHERE id = 'r1'"),
            UTC
        );
    }

    /// Tables with more rows than a page are converted to their last row, the
    /// array and the blocks table alike.
    #[test]
    fn converts_every_row_past_the_first_page() {
        let conn = conn_with(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, meta TEXT);
             CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, _block_type TEXT, data TEXT);",
        );
        let rows = PAGE_SIZE * 2 + 1;
        let local = local_meta();

        for i in 0..rows {
            let id = format!("r{i:05}");
            conn.0
                .execute(
                    "INSERT INTO posts_items VALUES (?1, 'p1', 0, ?2)",
                    [&id, &local],
                )
                .unwrap();
            conn.0
                .execute(
                    "INSERT INTO posts_content VALUES (?1, 'p1', 0, 'event', ?2)",
                    [&id, &local],
                )
                .unwrap();
        }

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            items_with_meta(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new("event", vec![starts()])])
                .build(),
        ];
        convert_if_needed(&conn, &registry_with(def)).unwrap();

        for sql in [
            "SELECT COUNT(*) FROM posts_items WHERE json_extract(meta, '$.starts') = ?1",
            "SELECT COUNT(*) FROM posts_content WHERE json_extract(data, '$.starts') = ?1",
        ] {
            let count: i64 = conn.0.query_row(sql, [UTC], |r| r.get(0)).unwrap();
            assert_eq!(count, i64::try_from(rows).unwrap(), "{sql}");
        }
    }

    /// The meta rows of the conversion this one replaced are removed, so no
    /// orphaned gate outlives it.
    #[test]
    fn removes_the_replaced_conversions_meta_rows() {
        let conn = conn_with("CREATE TABLE posts (id TEXT PRIMARY KEY);");
        conn.0
            .execute_batch(
                "INSERT INTO _crap_meta VALUES ('nested_timezone_dates:posts', '1');
                 INSERT INTO _crap_meta VALUES ('nested_timezone_dates:site', '1');",
            )
            .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![items_with_meta()];
        let mut site = GlobalDefinition::new("site");
        site.fields = vec![items_with_meta()];
        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def);
        shared.write().unwrap().register_global(site);
        let registry = (*Registry::snapshot(&shared)).clone();

        convert_if_needed(&conn, &registry).unwrap();

        for key in ["nested_timezone_dates:posts", "nested_timezone_dates:site"] {
            assert_eq!(meta::get(&conn, key).unwrap(), None, "{key}");
        }
    }

    /// The gate moved from the slug to the table. A collection's slug is its
    /// table, so its row just carried on; a GLOBAL's old `:{slug}` row named no
    /// pass any more and outlived the conversion forever.
    #[test]
    fn removes_a_globals_slug_keyed_gate() {
        let conn = conn_with(
            "CREATE TABLE _global_site (id TEXT PRIMARY KEY);
             CREATE TABLE _global_site_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, meta TEXT);",
        );
        conn.0
            .execute_batch("INSERT INTO _crap_meta VALUES ('nested_values:site', '1:stale');")
            .unwrap();

        let mut def = GlobalDefinition::new("site");
        def.fields = vec![items_with_meta()];
        let shared = Registry::shared();
        shared.write().unwrap().register_global(def);
        let registry = (*Registry::snapshot(&shared)).clone();

        convert_if_needed(&conn, &registry).unwrap();

        assert_eq!(meta::get(&conn, "nested_values:site").unwrap(), None);
        assert!(
            meta::get(&conn, &meta_key("_global_site"))
                .unwrap()
                .is_some(),
            "the table-keyed gate is the live one"
        );
    }

    /// Existing group-in-array-row JSON and blocks `data` are converted once;
    /// the gate stops a second pass.
    #[test]
    fn converts_existing_nested_rows_once() {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE posts (id TEXT PRIMARY KEY);
                 CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, meta TEXT);
                 CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, _block_type TEXT, data TEXT);",
            )
            .unwrap();
        let local =
            json!({ "starts": LOCAL, "starts_tz": "Europe/Berlin", "featured": "on" }).to_string();
        conn.0
            .execute(
                "INSERT INTO posts_items VALUES ('r1', 'p1', 0, ?1)",
                [&local],
            )
            .unwrap();
        conn.0
            .execute(
                "INSERT INTO posts_content VALUES ('b1', 'p1', 0, 'event', ?1)",
                [&local],
            )
            .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![starts()])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "event",
                    vec![
                        starts(),
                        FieldDefinition::builder("featured", FieldType::Checkbox).build(),
                    ],
                )])
                .build(),
        ];
        let registry = registry_with(def);

        convert_if_needed(&conn, &registry).unwrap();

        assert_eq!(
            stored_starts(&conn, "SELECT meta FROM posts_items WHERE id = 'r1'"),
            UTC
        );
        assert_eq!(
            stored_starts(&conn, "SELECT data FROM posts_content WHERE id = 'b1'"),
            UTC
        );
        let data: String = conn
            .0
            .query_row("SELECT data FROM posts_content WHERE id = 'b1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&data).unwrap()["featured"],
            json!(true)
        );
        let gate = meta::get(&conn, &meta_key("posts")).unwrap().unwrap();
        assert!(gate.starts_with(&format!("{MIGRATION_VERSION}:")), "{gate}");

        conn.0
            .execute(
                "UPDATE posts_content SET data = ?1 WHERE id = 'b1'",
                [&local],
            )
            .unwrap();
        convert_if_needed(&conn, &registry).unwrap();

        assert_eq!(
            stored_starts(&conn, "SELECT data FROM posts_content WHERE id = 'b1'"),
            LOCAL,
            "the gate must stop a second pass"
        );
    }

    /// Regression: the gate held the version alone, so a sub-field added to an
    /// array after a pass — or a collection whose join table already held rows
    /// written before its definition came back — was never converted. The gate
    /// fingerprints the join-table columns, so a changed shape runs it again.
    #[test]
    fn a_sub_field_added_later_runs_the_conversion_again() {
        let conn = conn_with(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, meta TEXT);",
        );
        conn.0
            .execute(
                "INSERT INTO posts_items VALUES ('r1', 'p1', 0, ?1)",
                [&local_meta()],
            )
            .unwrap();

        // The group holds only the date, so the checkbox beside it is left as
        // the admin form sent it.
        let mut without_checkbox = CollectionDefinition::new("posts");
        without_checkbox.fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![starts()])
                        .build(),
                ])
                .build(),
        ];
        convert_if_needed(&conn, &registry_with(without_checkbox)).unwrap();

        assert_eq!(
            stored_json(&conn, "SELECT meta FROM posts_items WHERE id = 'r1'")["featured"],
            json!("on"),
            "a field the definition doesn't hold yet stays as it was written"
        );

        let mut with_checkbox = CollectionDefinition::new("posts");
        with_checkbox.fields = vec![items_with_meta()];
        convert_if_needed(&conn, &registry_with(with_checkbox)).unwrap();

        assert_eq!(
            stored_json(&conn, "SELECT meta FROM posts_items WHERE id = 'r1'")["featured"],
            json!(true),
            "a sub-field added after a pass must run the conversion again"
        );
    }
}
