//! One-time conversion of timezone dates nested in JSON-stored rows to UTC.
//!
//! Dates inside blocks rows, groups inside array rows and nested array rows
//! used to be stored as the wall-clock digits the editor entered, while every
//! other timezone date is stored as UTC. Writes now convert them; this converts
//! the rows written before, once per collection or global. A value that already
//! carries an offset is left as it is, so a date can never be shifted twice.

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
        migrate::helpers::table_exists,
        query::{
            helpers::{global_table, join_table, prefixed_name},
            join::convert_timezone_dates,
        },
    },
};

use super::meta;

/// Stored as the meta value; bump to force a re-run after a change here.
const MIGRATION_VERSION: &str = "1";

fn meta_key(slug: &str) -> String {
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

/// Convert nested timezone dates for every collection and global not yet at
/// the current version.
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
    let key = meta_key(slug);
    if meta::get(conn, &key)?.as_deref() == Some(MIGRATION_VERSION) {
        return Ok(());
    }

    let mut join_fields = Vec::new();
    collect_join_fields(fields, "", &mut join_fields);

    let mut converted = 0;
    for field in &join_fields {
        converted += match field {
            JoinField::Array { key, sub } => {
                convert_array_table(conn, &join_table(table, key), sub)?
            }
            JoinField::Blocks { key, defs } => {
                convert_blocks_table(conn, &join_table(table, key), defs)?
            }
        };
    }

    if converted > 0 {
        info!("Converted nested timezone dates to UTC in {converted} row(s) of '{slug}'");
    }

    meta::upsert(conn, &key, MIGRATION_VERSION)
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
    let json_subs: Vec<&FieldDefinition> = flatten_array_sub_fields(sub)
        .into_iter()
        .filter(|sf| {
            matches!(
                field_children(sf),
                FieldChildren::Group(_) | FieldChildren::Array(_) | FieldChildren::Blocks(_)
            )
        })
        .collect();

    if json_subs.is_empty() || !table_exists(conn, table)? {
        return Ok(0);
    }

    let mut converted = 0;

    for sf in json_subs {
        let column = &sf.name;
        let rows = conn.query_all(
            &format!("SELECT id, \"{column}\" FROM \"{table}\" WHERE \"{column}\" IS NOT NULL"),
            &[],
        )?;

        for row in &rows {
            let (Some(id), Some(raw)) = (row.opt_text_at(0), row.opt_text_at(1)) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };

            let mut holder = Map::new();
            holder.insert(column.clone(), value);
            let before = holder.clone();

            convert_timezone_dates(slice::from_ref(sf), &mut holder);
            if holder == before {
                continue;
            }

            conn.execute(
                &format!(
                    "UPDATE \"{table}\" SET \"{column}\" = {} WHERE id = {}",
                    conn.placeholder(1),
                    conn.placeholder(2)
                ),
                &[DbValue::Text(holder[column].to_string()), DbValue::Text(id)],
            )
            .with_context(|| format!("Failed to convert nested dates in {table}.{column}"))?;
            converted += 1;
        }
    }

    Ok(converted)
}

/// Convert the `data` JSON of every row of a blocks join table, each with its
/// own block definition. Rows of a block type no longer defined are left alone.
fn convert_blocks_table(
    conn: &dyn DbConnection,
    table: &str,
    defs: &[BlockDefinition],
) -> Result<usize> {
    if !table_exists(conn, table)? {
        return Ok(0);
    }

    let rows = conn.query_all(
        &format!("SELECT id, _block_type, data FROM \"{table}\""),
        &[],
    )?;
    let mut converted = 0;

    for row in &rows {
        let (Some(id), Some(block_type), Some(raw)) =
            (row.opt_text_at(0), row.opt_text_at(1), row.opt_text_at(2))
        else {
            continue;
        };
        let Some(def) = defs.iter().find(|d| d.block_type == block_type) else {
            continue;
        };
        let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };

        let before = data.clone();
        convert_timezone_dates(&def.fields, &mut data);
        if data == before {
            continue;
        }

        conn.execute(
            &format!(
                "UPDATE \"{table}\" SET data = {} WHERE id = {}",
                conn.placeholder(1),
                conn.placeholder(2)
            ),
            &[
                DbValue::Text(Value::Object(data).to_string()),
                DbValue::Text(id),
            ],
        )
        .with_context(|| format!("Failed to convert nested dates in {table}"))?;
        converted += 1;
    }

    Ok(converted)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{CollectionDefinition, FieldType},
        db::InMemoryConn,
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
        let raw: String = conn.0.query_row(sql, [], |r| r.get(0)).unwrap();

        serde_json::from_str::<Value>(&raw).unwrap()["starts"].clone()
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
        let local = json!({ "starts": LOCAL, "starts_tz": "Europe/Berlin" }).to_string();
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
                .blocks(vec![BlockDefinition::new("event", vec![starts()])])
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
        assert_eq!(
            meta::get(&conn, &meta_key("posts")).unwrap().as_deref(),
            Some(MIGRATION_VERSION)
        );

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
}
