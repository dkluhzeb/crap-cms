//! A relationship or upload whose `has_many` flag flipped keeps its values.
//!
//! A has-one reference in a document's own table (top level, or inside a
//! group) stores its id in a column of that table; a has-many one stores its
//! ids in a junction table `{table}_{field}`. Turning `has_many` on or off
//! therefore moves where the values live, and without a carry every document
//! reads the field empty from then on — the stranded values stop counting as
//! references too, so their targets become deletable and the values can never
//! be recovered by flipping the flag back.
//!
//! Each boot compares the cardinality of every such field with the one the
//! last boot recorded in `_crap_meta` and carries the values across:
//!
//! - **has-one → has-many**: the junction is refilled from the column, one row
//!   per document (per locale when localized) at order 0.
//! - **has-many → has-one**: the column is refilled from the junction — only
//!   when no document holds more than one value (per locale). Otherwise the
//!   boot is refused, naming the documents, since picking one value would drop
//!   the others.
//!
//! The side the values left is kept, not cleared: the old column is reported
//! as an orphan column and the old junction as a leftover table, both removed
//! by `db cleanup`. Changing `localized` or polymorphism (one target / several) in the same
//! deploy as `has_many` is refused — the carry would have to reshape two things
//! at once. The pass runs before the reference counts are recounted, so the
//! counts follow the carried values.
//!
//! Unlike the one-time conversions this pass stays: any later change to a
//! definition can flip the flag again.

use std::collections::HashMap;

use anyhow::{Context as _, Result, bail};
use tracing::{info, warn};

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, FieldType, Registry, RelationshipConfig},
    db::{
        DbConnection, DbValue,
        migrate::{
            helpers::{Scan, for_each_row, get_table_columns, table_exists},
            meta,
        },
        query::{
            helpers::{
                global_table, join_table, locale_column, prefixed_name, quote_ident,
                walk_leaf_fields,
            },
            poly_ref,
        },
    },
};

/// The `_crap_meta` key recording a table's reference cardinalities.
fn meta_key(table: &str) -> String {
    format!("reference_cardinality:{table}")
}

/// How a reference field stores its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    has_many: bool,
    localized: bool,
    polymorphic: bool,
}

impl Shape {
    /// The recorded form: `many|one` with `+loc` / `+poly` markers.
    fn encode(self) -> String {
        let mut out = String::from(if self.has_many { "many" } else { "one" });

        if self.localized {
            out.push_str("+loc");
        }

        if self.polymorphic {
            out.push_str("+poly");
        }

        out
    }

    /// Read a recorded form back; `None` for anything else.
    fn decode(value: &str) -> Option<Self> {
        let mut parts = value.split('+');

        let has_many = match parts.next()? {
            "many" => true,
            "one" => false,
            _ => return None,
        };

        let markers: Vec<&str> = parts.collect();

        Some(Self {
            has_many,
            localized: markers.contains(&"loc"),
            polymorphic: markers.contains(&"poly"),
        })
    }
}

/// A relationship or upload field of a table's own storage (not inside an
/// array or blocks row), named by its `__`-joined path.
struct RefLeaf<'a> {
    path: String,
    field: &'a FieldDefinition,
    localized: bool,
}

impl RefLeaf<'_> {
    fn shape(&self) -> Shape {
        Shape {
            has_many: !self.field.has_parent_column(),
            localized: self.localized,
            polymorphic: self
                .field
                .relationship
                .as_ref()
                .is_some_and(RelationshipConfig::is_polymorphic),
        }
    }

    /// The columns a has-one value lives in, each with the locale its junction
    /// rows carry (`None` when the field is not localized).
    fn columns<'c>(
        &self,
        locale_config: &'c LocaleConfig,
    ) -> Result<Vec<(String, Option<&'c str>)>> {
        if !self.localized {
            return Ok(vec![(self.path.clone(), None)]);
        }

        locale_config
            .locales
            .iter()
            .map(|locale| Ok((locale_column(&self.path, locale)?, Some(locale.as_str()))))
            .collect()
    }
}

/// Every relationship and upload field stored in the table itself or its
/// junction — group prefixes applied, layout wrappers transparent, array and
/// blocks rows left out (their references are stored inside the row).
fn ref_leaves<'a>(fields: &'a [FieldDefinition], locale_config: &LocaleConfig) -> Vec<RefLeaf<'a>> {
    let mut leaves = Vec::new();

    let _ = walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if matches!(
                field.field_type,
                FieldType::Relationship | FieldType::Upload
            ) {
                leaves.push(RefLeaf {
                    path: prefixed_name(prefix, &field.name),
                    field,
                    localized: (inherited_localized || field.localized)
                        && locale_config.is_enabled(),
                });
            }

            Ok(())
        },
    );

    leaves
}

/// The recorded value of a table's leaves: `path=shape`, sorted.
fn shape_value(leaves: &[RefLeaf<'_>]) -> String {
    let mut parts: Vec<String> = leaves
        .iter()
        .map(|leaf| format!("{}={}", leaf.path, leaf.shape().encode()))
        .collect();
    parts.sort();

    parts.join(",")
}

/// Read a recorded value back into `path → shape`.
fn parse_shape_value(value: &str) -> HashMap<String, Shape> {
    value
        .split(',')
        .filter_map(|part| part.split_once('='))
        .filter_map(|(path, shape)| Some((path.to_string(), Shape::decode(shape)?)))
        .collect()
}

/// One table whose references may need carrying, and the locales its
/// columns are laid out for.
struct Owner<'a> {
    table: String,
    fields: &'a [FieldDefinition],
    locale_config: &'a LocaleConfig,
}

/// Every collection and global table of the registry.
fn owners<'a>(registry: &'a Registry, locale_config: &'a LocaleConfig) -> Vec<Owner<'a>> {
    let collections = registry.collections.iter().map(|(slug, def)| Owner {
        table: slug.to_string(),
        fields: &def.fields,
        locale_config,
    });
    let globals = registry.globals.iter().map(|(slug, def)| Owner {
        table: global_table(slug),
        fields: &def.fields,
        locale_config,
    });

    collections.chain(globals).collect()
}

/// Carry the values of every reference whose `has_many` flag flipped since the
/// last boot, then record the current cardinalities.
///
/// # Errors
///
/// Returns an error when a field turned has-one while a document holds more
/// than one value, when `has_many` changed together with `localized` or
/// polymorphism, or on a backend error.
pub(super) fn carry_if_needed(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    for owner in owners(registry, locale_config) {
        carry_owner(conn, &owner)?;
    }

    Ok(())
}

/// [`carry_if_needed`] for one table.
fn carry_owner(conn: &dyn DbConnection, owner: &Owner<'_>) -> Result<()> {
    let leaves = ref_leaves(owner.fields, owner.locale_config);
    let current = shape_value(&leaves);
    let key = meta_key(&owner.table);

    let previous = meta::get(conn, &key)?;

    if previous.as_deref() == Some(current.as_str()) {
        return Ok(());
    }

    // Nothing recorded yet: this boot only learns the shape.
    if let Some(previous) = previous {
        let previous = parse_shape_value(&previous);

        for leaf in &leaves {
            let Some(before) = previous.get(&leaf.path) else {
                continue;
            };

            carry_leaf(conn, owner, leaf, *before)?;
        }
    }

    meta::upsert(conn, &key, &current)
}

/// Carry one field's values when its cardinality flipped.
fn carry_leaf(
    conn: &dyn DbConnection,
    owner: &Owner<'_>,
    leaf: &RefLeaf<'_>,
    before: Shape,
) -> Result<()> {
    let now = leaf.shape();
    let table = owner.table.as_str();

    if before.has_many == now.has_many {
        return Ok(());
    }

    if before.localized != now.localized || before.polymorphic != now.polymorphic {
        bail!(
            "Field '{}' of '{table}' changed has_many together with localized or with its \
             number of target collections — its stored values can only be carried across one \
             of those changes at a time. Revert one of them, start once, then apply it.",
            leaf.path
        );
    }

    let junction = join_table(table, &leaf.path);

    if !table_exists(conn, &junction)? {
        return Ok(());
    }

    // Only the columns the table holds: a column `db cleanup` dropped has no
    // values left to carry.
    let existing = get_table_columns(conn, table)?;
    let columns: Vec<(String, Option<&str>)> = leaf
        .columns(owner.locale_config)?
        .into_iter()
        .filter(|(column, _)| existing.contains(column))
        .collect();

    if columns.is_empty() {
        return Ok(());
    }

    let carry = Carry {
        table,
        junction: &junction,
        leaf,
        columns,
    };

    if now.has_many {
        carry.column_to_junction(conn)
    } else {
        carry.junction_to_column(conn)
    }
}

/// One field's values on their way between its column(s) and its junction.
struct Carry<'a> {
    table: &'a str,
    junction: &'a str,
    leaf: &'a RefLeaf<'a>,
    /// The has-one columns the table holds, and the locale each one's
    /// junction rows carry.
    columns: Vec<(String, Option<&'a str>)>,
}

impl Carry<'_> {
    /// has-one → has-many: refill the junction from the column(s).
    fn column_to_junction(&self, conn: &dyn DbConnection) -> Result<()> {
        conn.execute(&format!("DELETE FROM {}", quote_ident(self.junction)), &[])
            .with_context(|| format!("Failed to clear '{}'", self.junction))?;

        let mut carried = 0usize;

        for (column, locale) in &self.columns {
            carried += self.copy_column(conn, column, *locale)?;
        }

        info!(
            "Carried {carried} value(s) of '{}.{}' into '{}' (turned has-many)",
            self.table, self.leaf.path, self.junction
        );

        Ok(())
    }

    /// Insert one junction row per document holding a value in `column`.
    fn copy_column(
        &self,
        conn: &dyn DbConnection,
        column: &str,
        locale: Option<&str>,
    ) -> Result<usize> {
        let polymorphic = self.leaf.shape().polymorphic;
        let insert = junction_insert_sql(conn, self.junction, polymorphic, locale.is_some());
        let columns = [column];
        let scan = Scan::builder(self.table, &columns).build();
        let mut carried = 0usize;

        for_each_row(conn, &scan, &mut |row| {
            let (Some(parent), Some(value)) = (row.opt_text_at(0), row.opt_text_at(1)) else {
                return Ok(());
            };

            let Some(params) = junction_params(&parent, &value, polymorphic, locale) else {
                warn!(
                    "Not carrying '{value}' of '{}.{column}' for {parent}: not a reference",
                    self.table
                );

                return Ok(());
            };

            conn.execute(&insert, &params)?;
            carried += 1;

            Ok(())
        })?;

        Ok(carried)
    }

    /// has-many → has-one: refill the column(s) from the junction, refusing
    /// when a document holds more than one value.
    fn junction_to_column(&self, conn: &dyn DbConnection) -> Result<()> {
        self.refuse_multiple_values(conn)?;

        for (column, locale) in &self.columns {
            let sql = self.refill_sql(conn, column, locale.is_some());
            let params: Vec<DbValue> = locale
                .map(|l| DbValue::Text(l.to_string()))
                .into_iter()
                .collect();

            conn.execute(&sql, &params).with_context(|| {
                format!(
                    "Failed to carry '{}' into '{}.{column}'",
                    self.junction, self.table
                )
            })?;
        }

        info!(
            "Carried '{}' into '{}.{}' (turned has-one)",
            self.junction, self.table, self.leaf.path
        );

        Ok(())
    }

    /// The UPDATE setting every document's `column` to its single junction
    /// value, or NULL when it has none. A localized column reads its locale's
    /// rows (placeholder 1).
    fn refill_sql(&self, conn: &dyn DbConnection, column: &str, localized: bool) -> String {
        let value = if self.leaf.shape().polymorphic {
            format!(
                "j.related_collection || '{}' || j.related_id",
                poly_ref::SEPARATOR
            )
        } else {
            "j.related_id".to_string()
        };

        let locale_clause = if localized {
            format!(" AND j._locale = {}", conn.placeholder(1))
        } else {
            String::new()
        };

        let table = quote_ident(self.table);

        format!(
            "UPDATE {table} SET {} = (SELECT {value} FROM {} j \
             WHERE j.parent_id = {table}.id{locale_clause})",
            quote_ident(column),
            quote_ident(self.junction)
        )
    }

    /// Fail when any document (per locale) holds more than one junction row.
    fn refuse_multiple_values(&self, conn: &dyn DbConnection) -> Result<()> {
        let group = if self.leaf.localized {
            "parent_id, _locale"
        } else {
            "parent_id"
        };

        let sql = format!(
            "SELECT DISTINCT parent_id FROM (SELECT {group} FROM {} GROUP BY {group} \
             HAVING COUNT(*) > 1) multi ORDER BY parent_id LIMIT 10",
            quote_ident(self.junction)
        );

        let ids: Vec<String> = conn
            .query_all(&sql, &[])?
            .iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect();

        if ids.is_empty() {
            return Ok(());
        }

        bail!(
            "Field '{}' of '{}' is has-one now, but documents hold more than one value in '{}' \
             (for example: {}). A has-one field keeps a single value: remove the extra values \
             — or turn has_many back on — then start again.",
            self.leaf.path,
            self.table,
            self.junction,
            ids.join(", ")
        )
    }
}

/// The INSERT of one carried junction row: parent, id, order 0, and the
/// target collection / locale when the junction has those columns.
fn junction_insert_sql(
    conn: &dyn DbConnection,
    junction: &str,
    polymorphic: bool,
    localized: bool,
) -> String {
    let mut columns = vec!["parent_id", "related_id"];

    if polymorphic {
        columns.push("related_collection");
    }

    if localized {
        columns.push("_locale");
    }

    let placeholders: Vec<String> = (1..=columns.len()).map(|i| conn.placeholder(i)).collect();

    format!(
        "INSERT INTO {} ({}, _order) VALUES ({}, 0)",
        quote_ident(junction),
        columns.join(", "),
        placeholders.join(", ")
    )
}

/// The parameters of [`junction_insert_sql`] for a stored has-one `value`;
/// `None` when a polymorphic value is not `collection/id`.
fn junction_params(
    parent: &str,
    value: &str,
    polymorphic: bool,
    locale: Option<&str>,
) -> Option<Vec<DbValue>> {
    if value.is_empty() {
        return None;
    }

    let mut params = vec![DbValue::Text(parent.to_string())];

    if polymorphic {
        let (collection, id) = poly_ref::parse(value)?;
        params.push(DbValue::Text(id));
        params.push(DbValue::Text(collection));
    } else {
        params.push(DbValue::Text(value.to_string()));
    }

    if let Some(locale) = locale {
        params.push(DbValue::Text(locale.to_string()));
    }

    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::BlockDefinition;
    use crate::db::{
        DbPool,
        migrate::{collection::test_helpers::*, sync_all},
    };

    fn author(has_many: bool) -> FieldDefinition {
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", has_many))
            .build()
    }

    fn registry(author: FieldDefinition) -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(simple_collection("users", vec![]));
        registry.register_collection(simple_collection("posts", vec![author]));

        registry
    }

    fn ref_count(pool: &DbPool, id: &str) -> i64 {
        pool.get()
            .unwrap()
            .query_one(
                "SELECT _ref_count FROM users WHERE id = ?1",
                &[DbValue::Text(id.into())],
            )
            .unwrap()
            .unwrap()
            .get_i64("_ref_count")
            .unwrap()
    }

    fn junction_rows(pool: &DbPool) -> Vec<(String, String)> {
        pool.get()
            .unwrap()
            .query_all(
                "SELECT parent_id, related_id FROM posts_author ORDER BY parent_id",
                &[],
            )
            .unwrap()
            .iter()
            .map(|r| (r.opt_text_at(0).unwrap(), r.opt_text_at(1).unwrap()))
            .collect()
    }

    fn column_value(pool: &DbPool, id: &str) -> Option<String> {
        pool.get()
            .unwrap()
            .query_one(
                "SELECT author FROM posts WHERE id = ?1",
                &[DbValue::Text(id.into())],
            )
            .unwrap()
            .unwrap()
            .opt_text_at(0)
    }

    /// Regression: turning `has_many` on for a top-level relationship left every
    /// value in the old column — each post read `author = []`, and each user
    /// lost the reference count the posts held. The values move into the
    /// junction and keep counting.
    #[test]
    fn turning_has_many_on_carries_the_column_into_the_junction() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry(author(false)), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO users (id) VALUES ('u1'), ('u2');
                 INSERT INTO posts (id, author) VALUES ('p1', 'u1'), ('p2', 'u2'), ('p3', NULL);",
            )
            .unwrap();

        sync_all(&pool, &registry(author(true)), &no_locale()).unwrap();

        assert_eq!(
            junction_rows(&pool),
            vec![("p1".into(), "u1".into()), ("p2".into(), "u2".into())]
        );
        assert_eq!(
            ref_count(&pool, "u1"),
            1,
            "the carried value keeps counting"
        );
    }

    /// The reverse: a has-many field turned has-one fills its column from the
    /// junction when every document holds at most one value.
    #[test]
    fn turning_has_many_off_carries_the_junction_into_the_column() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry(author(true)), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO users (id) VALUES ('u1');
                 INSERT INTO posts (id) VALUES ('p1'), ('p2');
                 INSERT INTO posts_author (parent_id, related_id, _order) VALUES ('p1', 'u1', 0);",
            )
            .unwrap();

        sync_all(&pool, &registry(author(false)), &no_locale()).unwrap();

        assert_eq!(column_value(&pool, "p1").as_deref(), Some("u1"));
        assert_eq!(column_value(&pool, "p2"), None);
        assert_eq!(ref_count(&pool, "u1"), 1);
    }

    /// A document holding two values cannot turn has-one without losing one:
    /// the sync is refused, naming it, and nothing changes.
    #[test]
    fn turning_has_many_off_with_several_values_is_refused() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry(author(true)), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO users (id) VALUES ('u1'), ('u2');
                 INSERT INTO posts (id) VALUES ('p1');
                 INSERT INTO posts_author (parent_id, related_id, _order) \
                     VALUES ('p1', 'u1', 0), ('p1', 'u2', 1);",
            )
            .unwrap();

        let err = sync_all(&pool, &registry(author(false)), &no_locale())
            .unwrap_err()
            .to_string();

        assert!(err.contains("p1") && err.contains("author"), "{err}");
        assert_eq!(junction_rows(&pool).len(), 2, "nothing changed");
    }

    /// Flipping back and forth carries the current values each time: an edit
    /// made while has-one is what the junction holds afterwards.
    #[test]
    fn flipping_back_carries_the_latest_values() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry(author(true)), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO users (id) VALUES ('u1'), ('u2');
                 INSERT INTO posts (id) VALUES ('p1');
                 INSERT INTO posts_author (parent_id, related_id, _order) VALUES ('p1', 'u1', 0);",
            )
            .unwrap();

        sync_all(&pool, &registry(author(false)), &no_locale()).unwrap();
        pool.get()
            .unwrap()
            .execute("UPDATE posts SET author = 'u2' WHERE id = 'p1'", &[])
            .unwrap();
        sync_all(&pool, &registry(author(true)), &no_locale()).unwrap();

        assert_eq!(junction_rows(&pool), vec![("p1".into(), "u2".into())]);
    }

    /// A polymorphic value round-trips through `collection/id` and the
    /// junction's `related_collection`.
    #[test]
    fn polymorphic_values_carry_both_ways() {
        let poly = |has_many: bool| {
            let mut rc = RelationshipConfig::new("users", has_many);
            rc.polymorphic = vec!["users".into(), "pages".into()];

            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(rc)
                .build()
        };

        let reg = |has_many: bool| {
            let mut registry = registry(poly(has_many));
            registry.register_collection(simple_collection("pages", vec![]));

            registry
        };

        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &reg(false), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO pages (id) VALUES ('g1');
                 INSERT INTO posts (id, author) VALUES ('p1', 'pages/g1');",
            )
            .unwrap();

        sync_all(&pool, &reg(true), &no_locale()).unwrap();

        let row = pool
            .get()
            .unwrap()
            .query_one(
                "SELECT related_id, related_collection FROM posts_author",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(row.opt_text_at(0).as_deref(), Some("g1"));
        assert_eq!(row.opt_text_at(1).as_deref(), Some("pages"));

        pool.get()
            .unwrap()
            .execute("UPDATE posts SET author = NULL", &[])
            .unwrap();
        sync_all(&pool, &reg(false), &no_locale()).unwrap();

        assert_eq!(column_value(&pool, "p1").as_deref(), Some("pages/g1"));
    }

    /// A localized field carries each locale's value into its own rows.
    #[test]
    fn localized_values_carry_per_locale() {
        let localized = |has_many: bool| {
            let mut field = author(has_many);
            field.localized = true;

            field
        };

        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry(localized(false)), &locale_en_de()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                "INSERT INTO users (id) VALUES ('u1'), ('u2');
                 INSERT INTO posts (id, author__en, author__de) VALUES ('p1', 'u1', 'u2');",
            )
            .unwrap();

        sync_all(&pool, &registry(localized(true)), &locale_en_de()).unwrap();

        let rows: Vec<(String, String)> = pool
            .get()
            .unwrap()
            .query_all(
                "SELECT _locale, related_id FROM posts_author ORDER BY _locale",
                &[],
            )
            .unwrap()
            .iter()
            .map(|r| (r.opt_text_at(0).unwrap(), r.opt_text_at(1).unwrap()))
            .collect();

        assert_eq!(
            rows,
            vec![("de".into(), "u2".into()), ("en".into(), "u1".into())]
        );
    }

    /// Changing `has_many` and `localized` in one go is refused rather than
    /// guessed at.
    #[test]
    fn changing_has_many_with_localized_is_refused() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry(author(false)), &locale_en_de()).unwrap();

        let mut both = author(true);
        both.localized = true;

        let err = sync_all(&pool, &registry(both), &locale_en_de())
            .unwrap_err()
            .to_string();

        assert!(err.contains("one of those changes"), "{err}");
    }

    #[test]
    fn shapes_round_trip_through_their_recorded_form() {
        for has_many in [false, true] {
            for localized in [false, true] {
                for polymorphic in [false, true] {
                    let shape = Shape {
                        has_many,
                        localized,
                        polymorphic,
                    };

                    assert_eq!(Shape::decode(&shape.encode()), Some(shape));
                }
            }
        }

        assert_eq!(Shape::decode("sometimes"), None);
    }

    /// A table seen for the first time only records its shape.
    #[test]
    fn the_first_boot_records_the_shape() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &registry(author(false)), &no_locale()).unwrap();

        let conn = pool.get().unwrap();
        assert_eq!(
            meta::get(&conn, &meta_key("posts")).unwrap().as_deref(),
            Some("author=one")
        );
    }

    /// A reference inside an array row is stored in the row, not the table:
    /// only the group's reference is the table's own.
    #[test]
    fn leaves_inside_array_rows_are_not_the_tables_own() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![author(false)])
                .build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![author(true)])
                .build(),
        ];
        let leaves = ref_leaves(&fields, &no_locale());
        let paths: Vec<&str> = leaves.iter().map(|l| l.path.as_str()).collect();

        assert_eq!(paths, vec!["meta__author"]);
    }

    /// `posts` holding `author` (of the given cardinality) inside an `items`
    /// array row and inside a `hero` block.
    fn row_registry(has_many: bool) -> Registry {
        let items = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![author(has_many)])
            .build();
        let content = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new("hero", vec![author(has_many)])])
            .build();

        let mut registry = Registry::new();
        registry.register_collection(simple_collection("users", vec![]));
        registry.register_collection(simple_collection("posts", vec![items, content]));

        registry
    }

    fn row_text(pool: &DbPool, sql: &str) -> Option<String> {
        pool.get()
            .unwrap()
            .query_one(sql, &[])
            .unwrap()
            .and_then(|row| row.opt_text_at(0))
    }

    /// Regression: turning `has_many` off for a reference inside an array or
    /// blocks row left each row's one-element list behind — the has-one reader
    /// and the recount saw no reference, so the target lost its count and
    /// could be hard-deleted while rows still named it. The rows hold the
    /// single id again and keep counting.
    #[test]
    fn turning_has_many_off_inside_rows_keeps_the_single_values() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &row_registry(true), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                r#"INSERT INTO users (id) VALUES ('u1'), ('u2');
                   INSERT INTO posts (id) VALUES ('p1');
                   INSERT INTO posts_items (id, parent_id, _order, author)
                     VALUES ('i1', 'p1', 0, '["u1"]'), ('i2', 'p1', 1, '[]');
                   INSERT INTO posts_content (id, parent_id, _order, _block_type, data)
                     VALUES ('b1', 'p1', 0, 'hero', '{"author":["u2"]}');"#,
            )
            .unwrap();

        sync_all(&pool, &row_registry(false), &no_locale()).unwrap();

        assert_eq!(
            row_text(&pool, "SELECT author FROM posts_items WHERE id = 'i1'").as_deref(),
            Some("u1")
        );
        assert_eq!(
            row_text(&pool, "SELECT author FROM posts_items WHERE id = 'i2'"),
            None
        );
        assert_eq!(
            row_text(&pool, "SELECT data FROM posts_content").as_deref(),
            Some(r#"{"author":"u2"}"#)
        );
        assert_eq!(ref_count(&pool, "u1"), 1, "the array row keeps counting");
        assert_eq!(ref_count(&pool, "u2"), 1, "the block keeps counting");
    }

    /// The other direction inside rows: single ids become one-element lists.
    #[test]
    fn turning_has_many_on_inside_rows_lists_the_single_values() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &row_registry(false), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                r#"INSERT INTO users (id) VALUES ('u1'), ('u2');
                   INSERT INTO posts (id) VALUES ('p1');
                   INSERT INTO posts_items (id, parent_id, _order, author)
                     VALUES ('i1', 'p1', 0, 'u1');
                   INSERT INTO posts_content (id, parent_id, _order, _block_type, data)
                     VALUES ('b1', 'p1', 0, 'hero', '{"author":"u2"}');"#,
            )
            .unwrap();

        sync_all(&pool, &row_registry(true), &no_locale()).unwrap();

        assert_eq!(
            row_text(&pool, "SELECT author FROM posts_items").as_deref(),
            Some(r#"["u1"]"#)
        );
        assert_eq!(
            row_text(&pool, "SELECT data FROM posts_content").as_deref(),
            Some(r#"{"author":["u2"]}"#)
        );
        assert_eq!(ref_count(&pool, "u1"), 1);
        assert_eq!(ref_count(&pool, "u2"), 1);
    }

    /// A row holding two ids cannot turn has-one without losing one: the sync
    /// is refused, naming the row, and the row keeps both.
    #[test]
    fn turning_has_many_off_inside_rows_with_several_values_is_refused() {
        let (_dir, pool) = in_memory_pool();
        sync_all(&pool, &row_registry(true), &no_locale()).unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                r#"INSERT INTO users (id) VALUES ('u1'), ('u2');
                   INSERT INTO posts (id) VALUES ('p1');
                   INSERT INTO posts_items (id, parent_id, _order, author)
                     VALUES ('i1', 'p1', 0, '["u1","u2"]');"#,
            )
            .unwrap();

        let err = format!(
            "{:#}",
            sync_all(&pool, &row_registry(false), &no_locale()).unwrap_err()
        );

        assert!(
            err.contains("has-one") && err.contains("i1") && err.contains("p1"),
            "{err}"
        );
        assert_eq!(
            row_text(&pool, "SELECT author FROM posts_items").as_deref(),
            Some(r#"["u1","u2"]"#)
        );
    }
}
