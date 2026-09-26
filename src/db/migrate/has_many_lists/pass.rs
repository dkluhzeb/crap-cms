//! Running the pass: per collection and global, gated on the shape of its
//! has-many fields, every covered column is brought to its list form a page
//! at a time.

use std::slice;

use anyhow::{Context as _, Result, bail};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::{
        DbConnection, DbRow, DbValue,
        migrate::{
            helpers::{Scan, for_each_row, update_by_id, versioned_fingerprint},
            meta, search_index,
        },
    },
};

use super::{
    columns::{Column, Stored, Target, list_columns, targets},
    values::{Change, column_change, json_change},
};

/// Leads the meta value; bump to run the pass again everywhere after a change
/// here. The rest of the value fingerprints the has-many fields a pass covered,
/// so a field switched to `has_many` or retyped later runs it again.
const PASS_VERSION: &str = "2";

/// The gate of one collection or global, keyed by its table.
fn meta_key(table: &str) -> String {
    format!("has_many_lists:{table}")
}

/// What a pass did: the rows it rewrote, and the values it could not.
#[derive(Default)]
struct Pass {
    rewritten: usize,
    rejected: Vec<String>,
}

/// Bring the has-many lists of every collection and global whose has-many
/// fields changed shape since the last pass to their list form.
///
/// # Errors
///
/// Returns an error naming the documents when a value holds nothing of its
/// field's type, or a backend error if a SELECT, an UPDATE, or the meta upsert
/// fails.
pub(in crate::db::migrate) fn normalize_if_needed(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    for target in targets(registry) {
        normalize_one(conn, &target, locale_config)?;
    }

    Ok(())
}

fn normalize_one(
    conn: &dyn DbConnection,
    target: &Target<'_>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let columns = list_columns(target, locale_config)?;
    if columns.is_empty() {
        return Ok(());
    }

    let key = meta_key(&target.table);
    let gate = gate_value(&columns);
    if meta::get(conn, &key)?.as_deref() == Some(gate.as_str()) {
        return Ok(());
    }

    let mut pass = Pass::default();
    for column in &columns {
        normalize_column(conn, column, &mut pass)?;
    }

    if !pass.rejected.is_empty() {
        bail!(
            "Values of fields in '{}' don't fit their field's stored shape (a number list holds \
             only numbers, a has-one reference one id):\n  {}\nChange or remove these values, \
             or the field definition, then start again — see \"Changing a definition that has \
             data\" in the database documentation.",
            target.slug,
            pass.rejected.join("\n  ")
        );
    }

    if pass.rewritten > 0 {
        info!(
            "Stored {} has-many value(s) of '{}' as lists",
            pass.rewritten, target.slug
        );
    }

    // The rewrite bypassed the per-write search upsert.
    if pass.rewritten > 0 && target.collection {
        search_index::invalidate(conn, target.slug)?;
    }

    meta::upsert(conn, &key, &gate)
}

/// `{version}:{fingerprint}` of the columns a pass covers.
fn gate_value(columns: &[Column]) -> String {
    let parts: Vec<String> = columns
        .iter()
        .map(|c| format!("{}.{}={}", c.table, c.name, c.stored.signature()))
        .collect();

    versioned_fingerprint(PASS_VERSION, &parts)
}

/// Bring the values of one column to their list form, a page at a time,
/// recording the ones that can't be.
fn normalize_column(conn: &dyn DbConnection, column: &Column, pass: &mut Pass) -> Result<()> {
    let selected = scanned_columns(column);
    let scan = Scan::builder(&column.table, &selected).build();
    let update = update_by_id(conn, &column.table, &column.name);

    for_each_row(conn, &scan, &mut |row| {
        let Some(scanned) = ScannedRow::read(column, row) else {
            return Ok(());
        };

        match scanned.change(column) {
            Change::Keep => {}
            Change::Store(value) => {
                conn.execute(&update, &[value, DbValue::Text(scanned.id)])
                    .with_context(|| {
                        format!("Failed to store {}.{} as a list", column.table, column.name)
                    })?;
                pass.rewritten += 1;
            }
            Change::Rejected(values) => {
                let at = scanned.location(column);
                pass.rejected
                    .extend(values.into_iter().map(|value| format!("{at}: {value}")));
            }
        }

        Ok(())
    })
}

/// The columns a pass reads beside `id`: the document a join-table row belongs
/// to, a blocks row's type — it decides which block definition the row's
/// `data` is read with — and the value last, the one the scan requires to be
/// non-NULL.
fn scanned_columns(column: &Column) -> Vec<&str> {
    let mut selected = Vec::new();

    if column.join {
        selected.push("parent_id");
    }

    if matches!(column.stored, Stored::Blocks(_)) {
        selected.push("_block_type");
    }

    selected.push(column.name.as_str());

    selected
}

/// A row as a pass reads it.
struct ScannedRow<'r> {
    id: String,
    parent: Option<String>,
    block_type: Option<String>,
    value: &'r DbValue,
}

impl<'r> ScannedRow<'r> {
    /// The row's columns, in the order [`scanned_columns`] lists them.
    fn read(column: &Column, row: &'r DbRow) -> Option<Self> {
        let value_at = scanned_columns(column).len();
        let blocks = matches!(column.stored, Stored::Blocks(_));

        Some(Self {
            id: row.opt_text_at(0)?,
            parent: column.join.then(|| row.opt_text_at(1)).flatten(),
            block_type: blocks.then(|| row.opt_text_at(value_at - 1)).flatten(),
            value: row.get_value(value_at)?,
        })
    }

    /// What the row's value needs to be in its list form.
    fn change(&self, column: &Column) -> Change {
        match &column.stored {
            Stored::List(field) => column_change(field, self.value, column.place()),
            Stored::Json(field) => json_change(
                self.value,
                slice::from_ref(field.as_ref()),
                Some(field.name.as_str()),
            ),
            Stored::Blocks(defs) => {
                let def = defs
                    .iter()
                    .find(|d| Some(d.block_type.as_str()) == self.block_type.as_deref());

                def.map_or(Change::Keep, |def| {
                    json_change(self.value, &def.fields, None)
                })
            }
        }
    }

    /// Where the row sits, for an error naming it.
    fn location(&self, column: &Column) -> String {
        let place = format!("{}.{}", column.table, column.name);

        match &self.parent {
            Some(parent) => format!("{place}, document '{parent}', row '{}'", self.id),
            None => format!("{place}, document '{}'", self.id),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, from_str, json};

    use super::*;
    use crate::{
        core::{
            BlockDefinition, CollectionDefinition, FieldDefinition, FieldType, GlobalDefinition,
            RelationshipConfig,
        },
        db::{InMemoryConn, migrate::helpers::PAGE_SIZE},
    };

    fn registry_with(def: CollectionDefinition) -> Registry {
        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def);

        (*Registry::snapshot(&shared)).clone()
    }

    fn locales() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn list(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, field_type)
            .has_many(true)
            .build()
    }

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    /// A has-many relationship to `collection` — or, given several, a
    /// polymorphic one.
    fn references(name: &str, collections: &[&str]) -> FieldDefinition {
        let mut config = RelationshipConfig::new(collections[0], true);

        if collections.len() > 1 {
            config.polymorphic = collections.iter().map(|c| (*c).into()).collect();
        }

        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(config)
            .build()
    }

    /// A collection with has-many lists in every place one is stored: its own
    /// column, per locale, inside a group, an array row's column, a group in
    /// an array row, and a block — and has-many references inside rows.
    fn posts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            list("tags", FieldType::Text),
            list("scores", FieldType::Number),
            FieldDefinition::builder("labels", FieldType::Select)
                .has_many(true)
                .localized(true)
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![list("keywords", FieldType::Text)])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    list("sizes", FieldType::Number),
                    references("related", &["tags"]),
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![list("notes", FieldType::Text), text("plain")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "quote",
                    vec![
                        list("refs", FieldType::Text),
                        references("links", &["posts", "pages"]),
                    ],
                )])
                .build(),
        ];

        def
    }

    fn posts_tables(conn: &InMemoryConn) {
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE posts (id TEXT PRIMARY KEY, tags TEXT, scores REAL,
                   labels__en TEXT, labels__de TEXT, seo__keywords TEXT);
                 CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER,
                   sizes TEXT, related TEXT, meta TEXT);
                 CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER,
                   _block_type TEXT, data TEXT);",
            )
            .unwrap();
    }

    fn stored(conn: &InMemoryConn, sql: &str) -> Option<String> {
        conn.0.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn stored_json(conn: &InMemoryConn, sql: &str) -> Value {
        from_str(&stored(conn, sql).unwrap()).unwrap()
    }

    /// Single values stored before their field held a list become one-element
    /// lists everywhere a list is stored — a number column's own number too —
    /// blank text becomes NULL, and a list already in its form is left alone.
    #[test]
    fn stores_every_single_value_as_a_list() {
        let conn = InMemoryConn::open();
        posts_tables(&conn);
        conn.0
            .execute_batch(
                r#"INSERT INTO posts (id, tags, scores, labels__en, labels__de, seo__keywords)
                     VALUES ('p1', 'news', 5, 'a', '["b"]', '');
                   INSERT INTO posts (id, tags, scores) VALUES ('p2', '["x","y"]', '7');
                   INSERT INTO posts_items (id, parent_id, _order, sizes, related, meta)
                     VALUES ('i1', 'p1', 0, '2.5', 't1, t2', '{"notes":"hello","plain":"kept"}');
                   INSERT INTO posts_content (id, parent_id, _order, _block_type, data)
                     VALUES ('b1', 'p1', 0, 'quote', '{"refs":"one","links":"pages/x"}');"#,
            )
            .unwrap();

        normalize_if_needed(&conn, &registry_with(posts_def()), &locales()).unwrap();

        let at = |sql: &str| stored(&conn, sql);
        assert_eq!(
            at("SELECT tags FROM posts WHERE id = 'p1'").as_deref(),
            Some(r#"["news"]"#)
        );
        assert_eq!(
            at("SELECT scores FROM posts WHERE id = 'p1'").as_deref(),
            Some("[5]")
        );
        assert_eq!(
            at("SELECT labels__en FROM posts").as_deref(),
            Some(r#"["a"]"#)
        );
        assert_eq!(
            at("SELECT labels__de FROM posts").as_deref(),
            Some(r#"["b"]"#)
        );
        assert_eq!(at("SELECT seo__keywords FROM posts WHERE id = 'p1'"), None);
        assert_eq!(
            at("SELECT tags FROM posts WHERE id = 'p2'").as_deref(),
            Some(r#"["x","y"]"#)
        );
        assert_eq!(
            at("SELECT scores FROM posts WHERE id = 'p2'").as_deref(),
            Some("[7]")
        );
        assert_eq!(
            at("SELECT sizes FROM posts_items").as_deref(),
            Some("[2.5]")
        );
        assert_eq!(
            stored_json(&conn, "SELECT meta FROM posts_items"),
            json!({ "notes": ["hello"], "plain": "kept" })
        );
        assert_eq!(
            at("SELECT related FROM posts_items").as_deref(),
            Some(r#"["t1","t2"]"#)
        );
        assert_eq!(
            stored_json(&conn, "SELECT data FROM posts_content"),
            json!({ "refs": ["one"], "links": ["pages/x"] })
        );
    }

    /// Regression: a document column's single text was split at its commas
    /// when its field switched to `has_many` (`"Hello, world"` became two
    /// values). In a column it is one value; inside a row, where earlier admin
    /// forms stored lists as comma text, a comma list — of values or of
    /// references — still reads as its elements.
    #[test]
    fn column_text_is_one_value_row_text_a_comma_list() {
        let conn = InMemoryConn::open();
        posts_tables(&conn);
        conn.0
            .execute_batch(
                r#"INSERT INTO posts (id, tags, labels__en, seo__keywords)
                     VALUES ('p1', 'Hello, world', 'a, b', 'x,y');
                   INSERT INTO posts_items (id, parent_id, _order, sizes, related, meta)
                     VALUES ('i1', 'p1', 0, '1, 2.5', 't1,t2', '{"notes":"a, b"}');
                   INSERT INTO posts_content (id, parent_id, _order, _block_type, data)
                     VALUES ('b1', 'p1', 0, 'quote',
                       '{"refs":"one, two","links":"pages/x,posts/y"}');"#,
            )
            .unwrap();

        normalize_if_needed(&conn, &registry_with(posts_def()), &locales()).unwrap();

        let at = |sql: &str| stored(&conn, sql);
        assert_eq!(
            at("SELECT tags FROM posts").as_deref(),
            Some(r#"["Hello, world"]"#)
        );
        assert_eq!(
            at("SELECT labels__en FROM posts").as_deref(),
            Some(r#"["a, b"]"#)
        );
        assert_eq!(
            at("SELECT seo__keywords FROM posts").as_deref(),
            Some(r#"["x,y"]"#)
        );
        assert_eq!(
            at("SELECT sizes FROM posts_items").as_deref(),
            Some("[1,2.5]")
        );
        assert_eq!(
            at("SELECT related FROM posts_items").as_deref(),
            Some(r#"["t1","t2"]"#)
        );
        assert_eq!(
            stored_json(&conn, "SELECT meta FROM posts_items"),
            json!({ "notes": ["a", "b"] })
        );
        assert_eq!(
            stored_json(&conn, "SELECT data FROM posts_content"),
            json!({ "refs": ["one", "two"], "links": ["pages/x", "posts/y"] })
        );
    }

    /// A polymorphic reference entry that doesn't spell `collection/id` is no
    /// reference a list can hold: startup stops naming it.
    #[test]
    fn a_polymorphic_entry_without_its_collection_stops_startup() {
        let conn = InMemoryConn::open();
        posts_tables(&conn);
        conn.0
            .execute_batch(
                r#"INSERT INTO posts_content (id, parent_id, _order, _block_type, data)
                     VALUES ('b1', 'p1', 0, 'quote', '{"links":["pages/x","orphan"]}');"#,
            )
            .unwrap();

        let err = normalize_if_needed(&conn, &registry_with(posts_def()), &locales())
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("posts_content.data, document 'p1', row 'b1': links holds \"orphan\""),
            "{err}"
        );
    }

    /// The gate stops a second pass over an unchanged shape; switching another
    /// field to `has_many` runs it again.
    #[test]
    fn runs_again_only_when_the_has_many_fields_change() {
        let conn = InMemoryConn::open();
        posts_tables(&conn);
        conn.0
            .execute_batch("ALTER TABLE posts ADD COLUMN topic TEXT;")
            .unwrap();

        let mut def = posts_def();
        def.fields.push(text("topic"));
        normalize_if_needed(&conn, &registry_with(def.clone()), &locales()).unwrap();

        conn.0
            .execute_batch("INSERT INTO posts (id, tags, topic) VALUES ('p1', 'news', 'rust');")
            .unwrap();
        normalize_if_needed(&conn, &registry_with(def.clone()), &locales()).unwrap();
        assert_eq!(
            stored(&conn, "SELECT tags FROM posts").as_deref(),
            Some("news"),
            "an unchanged shape must not scan again"
        );

        let topic = def.fields.iter_mut().find(|f| f.name == "topic").unwrap();
        topic.has_many = true;
        normalize_if_needed(&conn, &registry_with(def), &locales()).unwrap();

        assert_eq!(
            stored(&conn, "SELECT topic FROM posts").as_deref(),
            Some(r#"["rust"]"#)
        );
        assert_eq!(
            stored(&conn, "SELECT tags FROM posts").as_deref(),
            Some(r#"["news"]"#)
        );
    }

    /// Text in a number list can't become a number: startup stops naming the
    /// column and the document — in a column of its own and in an array row's
    /// — and the gate isn't stored, so the next start checks again.
    #[test]
    fn text_in_a_number_list_stops_startup() {
        let conn = InMemoryConn::open();
        posts_tables(&conn);
        conn.0
            .execute_batch(
                r#"INSERT INTO posts (id, tags, scores) VALUES ('p1', 'news', 'abc');
                   INSERT INTO posts_items (id, parent_id, _order, sizes)
                     VALUES ('i1', 'p1', 0, '["1","big"]');"#,
            )
            .unwrap();

        let err = normalize_if_needed(&conn, &registry_with(posts_def()), &locales())
            .unwrap_err()
            .to_string();

        assert!(err.contains("'posts'"), "{err}");
        assert!(
            err.contains("posts.scores, document 'p1': holds \"abc\""),
            "{err}"
        );
        assert!(
            err.contains("posts_items.sizes, document 'p1', row 'i1': holds \"big\""),
            "{err}"
        );
        assert_eq!(meta::get(&conn, &meta_key("posts")).unwrap(), None);
    }

    /// Text in a number list nested inside a block's JSON is named with its
    /// field.
    #[test]
    fn text_in_a_nested_number_list_names_its_field() {
        let conn = InMemoryConn::open();
        posts_tables(&conn);
        conn.0
            .execute_batch(
                r#"INSERT INTO posts_content (id, parent_id, _order, _block_type, data)
                     VALUES ('b1', 'p1', 0, 'quote', '{"refs":"x","counts":"many"}');"#,
            )
            .unwrap();

        let mut def = posts_def();
        let content = def.fields.iter_mut().find(|f| f.name == "content").unwrap();
        content.blocks[0]
            .fields
            .push(list("counts", FieldType::Number));

        let err = normalize_if_needed(&conn, &registry_with(def), &locales())
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("posts_content.data, document 'p1', row 'b1': counts holds \"many\""),
            "{err}"
        );
    }

    /// A global's lists are kept too, under the global's own table.
    #[test]
    fn a_globals_lists_are_stored_as_lists() {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE _global_site (id TEXT PRIMARY KEY, tags TEXT);
                 INSERT INTO _global_site VALUES ('default', 'one');",
            )
            .unwrap();

        let mut def = GlobalDefinition::new("site");
        def.fields = vec![list("tags", FieldType::Text)];
        let shared = Registry::shared();
        shared.write().unwrap().register_global(def);
        let registry = (*Registry::snapshot(&shared)).clone();

        normalize_if_needed(&conn, &registry, &locales()).unwrap();

        assert_eq!(
            stored(&conn, "SELECT tags FROM _global_site").as_deref(),
            Some(r#"["one"]"#)
        );
    }

    /// The scan is paged and still reaches the last row.
    #[test]
    fn stores_every_row_past_the_first_page() {
        let conn = InMemoryConn::open();
        posts_tables(&conn);
        let rows = PAGE_SIZE * 2 + 1;

        for i in 0..rows {
            conn.0
                .execute(
                    "INSERT INTO posts (id, tags) VALUES (?1, 'news')",
                    [format!("r{i:05}")],
                )
                .unwrap();
        }

        normalize_if_needed(&conn, &registry_with(posts_def()), &locales()).unwrap();

        let count: i64 = conn
            .0
            .query_row(
                r#"SELECT COUNT(*) FROM posts WHERE tags = '["news"]'"#,
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, i64::try_from(rows).unwrap());
    }
}
