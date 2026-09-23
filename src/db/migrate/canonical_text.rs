//! **One-time conversion — removable after 0.1.0** (see [`super::one_time`]).
//!
//! Rewrite of stored email and text values to their canonical form.
//!
//! Email values are stored trimmed, lowercased and NFC-composed, Text and
//! Textarea values NFC-composed, and filters, uniqueness checks and logins
//! compare that form. Values stored before kept the form they were typed in, so
//! this rewrites them — in their columns and inside JSON-stored rows — once per
//! collection or global, and again whenever its set of such fields changes. Two
//! values of a unique field or unique index that are the same once canonical
//! would break uniqueness, so startup stops and names them instead of choosing
//! one.

use std::{collections::BTreeMap, slice};

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, from_str};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::{
        BlockDefinition, Builder, CollectionDefinition, FieldChildren, FieldDefinition, FieldType,
        GlobalDefinition, IndexDefinition, Registry, canonical_text, canonicalize_text_values,
        field_children, flatten_array_sub_fields, has_canonical_form,
    },
    db::{
        DbConnection, DbRow, DbValue,
        migrate::{
            collection::compound_index_columns,
            helpers::{
                Scan, block_paths, field_paths, for_each_row, holds_leaf, update_by_id,
                versioned_fingerprint,
            },
            meta,
        },
        query::{
            helpers::{global_table, join_table, prefixed_name, walk_leaf_fields},
            stored_columns,
        },
    },
};

/// Leads the meta value; bump to force a re-run after a change here. The rest
/// of the value fingerprints the columns a pass covered, so a field added or
/// retyped later runs it again.
const MIGRATION_VERSION: &str = "1";

/// The gate of one target, keyed by its table so a collection and a global of
/// the same slug can't share one — sharing it would leave the two rewriting
/// each other's gate on every boot.
fn meta_key(table: &str) -> String {
    format!("canonical_text:{table}")
}

/// The leaves a pass rewrites: the ones whose values have a canonical form.
fn keeps_canonical(field: &FieldDefinition) -> bool {
    has_canonical_form(&field.field_type)
}

/// How a stored column holds email or text values.
enum Stored {
    /// The column of an email or text field.
    Value(FieldType),
    /// JSON holding a field's value, which holds email or text values at some
    /// depth — a group, array or blocks field inside an array row.
    Json(Box<FieldDefinition>),
    /// A blocks table's `data`, per block type.
    Blocks(Vec<BlockDefinition>),
}

impl Stored {
    /// The canonical form of a stored value, or `None` when it already is.
    fn canonical(&self, raw: &str, block_type: Option<&str>) -> Option<String> {
        match self {
            Self::Value(field_type) => canonical_text(field_type, raw).filter(|c| c != raw),
            Self::Json(field) => {
                canonical_json(raw, slice::from_ref(field.as_ref()), Some(&field.name))
            }
            Self::Blocks(defs) => {
                let def = defs
                    .iter()
                    .find(|d| Some(d.block_type.as_str()) == block_type)?;

                canonical_json(raw, &def.fields, None)
            }
        }
    }

    /// What a pass rewrites in the column, for the gate's fingerprint.
    fn signature(&self) -> String {
        match self {
            Self::Value(field_type) => field_type.as_str().to_string(),
            Self::Json(field) => field_paths(slice::from_ref(field.as_ref()), &keeps_canonical),
            Self::Blocks(defs) => block_paths(defs, &keeps_canonical),
        }
    }
}

/// One stored column holding email or text values.
#[derive(Builder)]
struct Column {
    #[builder(required)]
    table: String,
    #[builder(required)]
    name: String,
    #[builder(required)]
    stored: Stored,
    /// Checked for values that collide once canonical.
    #[builder(default = false)]
    unique: bool,
}

/// A collection or global whose stored values are rewritten.
enum Target<'a> {
    Collection(&'a str, &'a CollectionDefinition),
    Global(&'a str, &'a GlobalDefinition),
}

impl Target<'_> {
    fn slug(&self) -> &str {
        match self {
            Self::Collection(slug, _) | Self::Global(slug, _) => slug,
        }
    }

    fn table(&self) -> String {
        match self {
            Self::Collection(slug, _) => (*slug).to_string(),
            Self::Global(slug, _) => global_table(slug),
        }
    }

    fn fields(&self) -> &[FieldDefinition] {
        match self {
            Self::Collection(_, def) => &def.fields,
            Self::Global(_, def) => &def.fields,
        }
    }

    /// Soft-deleted rows don't take part in a unique field's uniqueness.
    fn soft_delete(&self) -> bool {
        matches!(self, Self::Collection(_, def) if def.soft_delete)
    }

    /// Auth collections keep one account per `email`.
    fn auth(&self) -> bool {
        matches!(self, Self::Collection(_, def) if def.is_auth_collection())
    }

    /// The unique indexes spanning several columns.
    fn unique_indexes(&self) -> Vec<&IndexDefinition> {
        match self {
            Self::Collection(_, def) => def.indexes.iter().filter(|ix| ix.unique).collect(),
            Self::Global(..) => Vec::new(),
        }
    }
}

/// Rewrite stored email and text values of every collection and global whose
/// columns changed since the last pass.
///
/// # Errors
///
/// Returns an error naming the documents when two values of a unique field or
/// index collide once canonical, or a backend error if a SELECT, an UPDATE, or
/// the meta upsert fails.
pub(super) fn canonicalize_if_needed(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    for (slug, def) in &registry.collections {
        canonicalize_one(conn, &Target::Collection(slug, def), locale_config)?;
    }

    for (slug, def) in &registry.globals {
        canonicalize_one(conn, &Target::Global(slug, def), locale_config)?;
    }

    Ok(())
}

fn canonicalize_one(
    conn: &dyn DbConnection,
    target: &Target<'_>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table = target.table();

    // The gate was once keyed by slug. A collection's slug IS its table, so
    // only a global left a row behind — one that names no pass any more and
    // would sit in the database forever. Deleting an absent key changes
    // nothing, so this runs whether or not the pass does.
    if table != target.slug() {
        meta::delete(conn, &meta_key(target.slug()))?;
    }

    let columns = text_columns(target, locale_config)?;
    if columns.is_empty() {
        return Ok(());
    }

    let key = meta_key(&table);
    let gate = gate_value(&columns);
    if meta::get(conn, &key)?.as_deref() == Some(gate.as_str()) {
        return Ok(());
    }

    check_unique_values(conn, target, &columns, locale_config)?;

    let mut rewritten = 0;
    for column in &columns {
        rewritten += rewrite_column(conn, column)?;
    }

    if rewritten > 0 {
        info!(
            "Stored {rewritten} email or text value(s) of '{}' in canonical form",
            target.slug()
        );
    }

    meta::upsert(conn, &key, &gate)
}

/// `{version}:{fingerprint}` of the columns a pass covers.
fn gate_value(columns: &[Column]) -> String {
    let parts: Vec<String> = columns
        .iter()
        .map(|c| {
            let unique = if c.unique { "!" } else { "" };

            format!("{}.{}{unique}={}", c.table, c.name, c.stored.signature())
        })
        .collect();

    versioned_fingerprint(MIGRATION_VERSION, &parts)
}

/// Every column of a target holding email or text values: main-table columns
/// (one per locale when localized), array join-table columns — plain or JSON —
/// and blocks `data`.
fn text_columns(target: &Target<'_>, locale_config: &LocaleConfig) -> Result<Vec<Column>> {
    let mut columns = Vec::new();

    walk_leaf_fields(
        target.fields(),
        "",
        false,
        &mut |field, prefix, inherited| {
            let leaf = LeafField {
                field,
                prefix,
                inherited,
            };
            push_field_columns(&mut columns, target, &leaf, locale_config)
        },
    )?;

    Ok(columns)
}

/// A field as the leaf walk reaches it: its group `prefix`, and whether an
/// enclosing group is localized (`inherited`).
struct LeafField<'a> {
    field: &'a FieldDefinition,
    prefix: &'a str,
    inherited: bool,
}

/// The columns of one field that hold email or text values.
fn push_field_columns(
    columns: &mut Vec<Column>,
    target: &Target<'_>,
    leaf: &LeafField<'_>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let LeafField {
        field,
        prefix,
        inherited,
    } = *leaf;
    let main_table = target.table();
    let base = prefixed_name(prefix, &field.name);

    match field_children(field) {
        FieldChildren::Array(sub) => {
            push_array_columns(columns, &join_table(&main_table, &base), sub);
        }
        FieldChildren::Blocks(defs)
            if defs.iter().any(|d| holds_leaf(&d.fields, &keeps_canonical)) =>
        {
            let stored = Stored::Blocks(defs.to_vec());
            let table = join_table(&main_table, &base);
            columns.push(Column::builder(table, "data".to_string(), stored).build());
        }
        _ if has_canonical_form(&field.field_type) && field.has_parent_column() => {
            let unique = field.unique || (target.auth() && base == "email");
            let localized = inherited || field.localized;

            for name in stored_columns(&base, localized, locale_config)? {
                let stored = Stored::Value(field.field_type.clone());
                let column = Column::builder(main_table.clone(), name, stored);
                columns.push(column.unique(unique).build());
            }
        }
        _ => {}
    }

    Ok(())
}

/// The columns of an array join table holding email or text values: a
/// sub-field's own column, or the JSON of a group, array or blocks inside it.
fn push_array_columns(columns: &mut Vec<Column>, table: &str, sub: &[FieldDefinition]) {
    for sf in flatten_array_sub_fields(sub) {
        let stored = match field_children(sf) {
            FieldChildren::Leaf if has_canonical_form(&sf.field_type) => {
                Stored::Value(sf.field_type.clone())
            }
            FieldChildren::Group(_) | FieldChildren::Array(_) | FieldChildren::Blocks(_)
                if holds_leaf(slice::from_ref(sf), &keeps_canonical) =>
            {
                Stored::Json(Box::new(sf.clone()))
            }
            _ => continue,
        };

        columns.push(Column::builder(table.to_string(), sf.name.clone(), stored).build());
    }
}

/// Canonicalize the email and text values of a JSON value — the value of the
/// field `name` when given, otherwise an object of fields. `None` when nothing
/// changes or the value isn't JSON of that shape.
fn canonical_json(raw: &str, fields: &[FieldDefinition], name: Option<&str>) -> Option<String> {
    let value = from_str::<Value>(raw).ok()?;

    let mut data = match (name, value) {
        (Some(name), value) => Map::from_iter([(name.to_string(), value)]),
        (None, Value::Object(map)) => map,
        (None, _) => return None,
    };

    let before = data.clone();
    canonicalize_text_values(&mut data, fields);
    if data == before {
        return None;
    }

    let canonical = match name {
        Some(name) => data.remove(name)?,
        None => Value::Object(data),
    };

    Some(canonical.to_string())
}

/// Stop when two documents hold the same value — once canonical — in a unique
/// field or unique index.
fn check_unique_values(
    conn: &dyn DbConnection,
    target: &Target<'_>,
    columns: &[Column],
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut collisions = Vec::new();

    for c in columns.iter().filter(|c| c.unique) {
        collisions.extend(column_collisions(conn, c, target.soft_delete())?);
    }

    for index in target.unique_indexes() {
        let names = compound_index_columns(target.fields(), index, locale_config)?;
        let index = CompoundIndex::new(index, &names);
        collisions.extend(index_collisions(conn, target, &index, columns)?);
    }

    if collisions.is_empty() {
        return Ok(());
    }

    bail!(
        "Email and text values in '{}' are now compared in one canonical form — emails \
         ignoring case, both ignoring how accents were typed — and these documents share a \
         value that must be unique:\n  {}\nChange or remove the duplicates, then start again.",
        target.slug(),
        collisions.join("\n  ")
    )
}

/// One line per canonical value more than one document of a unique column
/// holds, naming the documents.
fn column_collisions(
    conn: &dyn DbConnection,
    column: &Column,
    active_only: bool,
) -> Result<Vec<String>> {
    let Stored::Value(field_type) = &column.stored else {
        return Ok(Vec::new());
    };

    let by_value = column_canonical_ids(conn, column, field_type, active_only)?;

    Ok(by_value
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(value, ids)| format!("{}: \"{value}\" — {}", column.name, ids.join(", ")))
        .collect())
}

/// The ids holding each canonical value of a unique column. Only the values —
/// not the rows — are held, so the scan costs what the column's distinct values
/// do however many rows there are. A NULL or blank value never collides.
fn column_canonical_ids(
    conn: &dyn DbConnection,
    column: &Column,
    field_type: &FieldType,
    active_only: bool,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut by_value: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let columns = [column.name.as_str()];
    let scan = Scan::builder(&column.table, &columns)
        .active_only(active_only)
        .build();

    for_each_row(conn, &scan, &mut |row| {
        let (Some(id), Some(raw)) = (row.opt_text_at(0), row.opt_text_at(1)) else {
            return Ok(());
        };

        let canonical = canonical_text(field_type, &raw).unwrap_or(raw);
        if !canonical.is_empty() {
            by_value.entry(canonical).or_default().push(id);
        }

        Ok(())
    })?;

    Ok(by_value)
}

/// A unique index spanning several fields, and the columns it spans.
struct CompoundIndex<'a> {
    def: &'a IndexDefinition,
    names: &'a [String],
}

impl<'a> CompoundIndex<'a> {
    fn new(def: &'a IndexDefinition, names: &'a [String]) -> Self {
        Self { def, names }
    }
}

/// One line per combination of canonical values more than one document holds
/// in a unique index over an email or text column, naming the documents. The
/// index has no trashed-row exemption, so trashed documents count.
fn index_collisions(
    conn: &dyn DbConnection,
    target: &Target<'_>,
    index: &CompoundIndex<'_>,
    columns: &[Column],
) -> Result<Vec<String>> {
    let CompoundIndex { def, names } = *index;
    let table = target.table();
    let types = index_column_types(&table, names, columns);

    if types.iter().all(Option::is_none) {
        return Ok(Vec::new());
    }

    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let by_values = index_canonical_ids(conn, &table, &names, &types)?;

    Ok(by_values
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(values, ids)| {
            let values: Vec<String> = values.iter().map(|v| format!("\"{v}\"")).collect();
            format!(
                "({}): {} — {}",
                def.fields.join(", "),
                values.join(", "),
                ids.join(", ")
            )
        })
        .collect())
}

/// The field type of each of an index's columns of `table` that holds email or
/// text values.
fn index_column_types<'c>(
    table: &str,
    names: &[String],
    columns: &'c [Column],
) -> Vec<Option<&'c FieldType>> {
    names
        .iter()
        .map(|name| {
            columns
                .iter()
                .find(|c| c.table == table && c.name == *name)
                .and_then(|c| match &c.stored {
                    Stored::Value(field_type) => Some(field_type),
                    _ => None,
                })
        })
        .collect()
}

/// A row's index values in canonical form — `None` when one is NULL, since
/// such a row never collides.
fn canonical_row(values: Vec<Option<String>>, types: &[Option<&FieldType>]) -> Option<Vec<String>> {
    values
        .into_iter()
        .zip(types)
        .map(|(value, field_type)| {
            value.map(|raw| {
                field_type
                    .and_then(|ft| canonical_text(ft, &raw))
                    .unwrap_or(raw)
            })
        })
        .collect()
}

/// The ids holding each combination of canonical index values. The scan leaves
/// out the rows whose last index column is NULL — [`canonical_row`] drops every
/// row holding a NULL anyway, so those could never have collided.
fn index_canonical_ids(
    conn: &dyn DbConnection,
    table: &str,
    names: &[&str],
    types: &[Option<&FieldType>],
) -> Result<BTreeMap<Vec<String>, Vec<String>>> {
    let mut by_values: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();

    // A scan leaves out the rows whose LAST listed column is NULL. That is what
    // this check wants: a NULL never collides in a unique index, and
    // `canonical_row` drops every row holding one anyway. Listing the columns
    // in another order, or reading the rows another way, would only add rows
    // that are then dropped.
    let scan = Scan::builder(table, names).build();

    for_each_row(conn, &scan, &mut |row| {
        let Some(id) = row.opt_text_at(0) else {
            return Ok(());
        };
        let values = (1..=names.len())
            .map(|i| row.get_value(i).and_then(value_text))
            .collect();

        if let Some(canonical) = canonical_row(values, types) {
            by_values.entry(canonical).or_default().push(id);
        }

        Ok(())
    })?;

    Ok(by_values)
}

/// A stored value as comparable text — `None` for NULL. An index can span
/// number and checkbox columns beside text ones.
fn value_text(value: &DbValue) -> Option<String> {
    match value {
        DbValue::Null => None,
        DbValue::Integer(n) => Some(n.to_string()),
        DbValue::Real(f) => Some(f.to_string()),
        DbValue::Text(s) => Some(s.clone()),
        DbValue::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
    }
}

/// Rewrite the values of one column that aren't canonical yet, a page at a
/// time. Returns how many rows changed.
fn rewrite_column(conn: &dyn DbConnection, column: &Column) -> Result<usize> {
    let selected = scanned_columns(column);
    let scan = Scan::builder(&column.table, &selected).build();
    let update = update_by_id(conn, &column.table, &column.name);
    let mut rewritten = 0;

    for_each_row(conn, &scan, &mut |row| {
        let Some((id, canonical)) = canonical_update(column, row) else {
            return Ok(());
        };

        conn.execute(&update, &[DbValue::Text(canonical), DbValue::Text(id)])
            .with_context(|| {
                format!(
                    "Failed to store {}.{} in canonical form",
                    column.table, column.name
                )
            })?;
        rewritten += 1;

        Ok(())
    })?;

    Ok(rewritten)
}

/// The columns a rewrite reads beside `id`: a blocks row's type decides which
/// block definition its `data` is canonicalized against, so it comes along. The
/// value is last, the one the scan requires to be non-NULL.
fn scanned_columns(column: &Column) -> Vec<&str> {
    match column.stored {
        Stored::Blocks(_) => vec!["_block_type", column.name.as_str()],
        _ => vec![column.name.as_str()],
    }
}

/// The id and canonical value of a scanned row, or `None` when its value is
/// canonical already.
fn canonical_update(column: &Column, row: &DbRow) -> Option<(String, String)> {
    let blocks = matches!(column.stored, Stored::Blocks(_));
    let (value_at, block_type) = if blocks {
        (2, row.opt_text_at(1))
    } else {
        (1, None)
    };

    let (id, raw) = (row.opt_text_at(0)?, row.opt_text_at(value_at)?);
    let canonical = column.stored.canonical(&raw, block_type.as_deref())?;

    Some((id, canonical))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::{InMemoryConn, migrate::helpers::PAGE_SIZE};

    fn registry_with(def: CollectionDefinition) -> Registry {
        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def);

        (*Registry::snapshot(&shared)).clone()
    }

    fn registry_with_global(def: GlobalDefinition) -> Registry {
        let shared = Registry::shared();
        shared.write().unwrap().register_global(def);

        (*Registry::snapshot(&shared)).clone()
    }

    /// The gate moved from the slug to the table. A collection's slug is its
    /// table, so its row carried on; a GLOBAL's old `:{slug}` row named no pass
    /// any more and outlived the conversion forever.
    #[test]
    fn removes_a_globals_slug_keyed_gate() {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE _global_site (id TEXT PRIMARY KEY, tagline TEXT);
                 INSERT INTO _global_site VALUES ('default', 'Rene\u{301}');
                 INSERT INTO _crap_meta VALUES ('canonical_text:site', '1:stale');",
            )
            .unwrap();

        let mut def = GlobalDefinition::new("site");
        def.fields = vec![text("tagline")];

        canonicalize_if_needed(&conn, &registry_with_global(def), &locales()).unwrap();

        assert_eq!(meta::get(&conn, "canonical_text:site").unwrap(), None);
        assert!(
            meta::get(&conn, &meta_key("_global_site"))
                .unwrap()
                .is_some(),
            "the table-keyed gate is the live one"
        );
        assert_eq!(
            stored(&conn, "SELECT tagline FROM _global_site"),
            "Ren\u{e9}",
            "the pass still runs"
        );
    }

    fn stored(conn: &InMemoryConn, sql: &str) -> String {
        conn.0.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn people_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("people");
        def.soft_delete = true;
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email)
                .unique(true)
                .build(),
            FieldDefinition::builder("work", FieldType::Email)
                .localized(true)
                .build(),
            text("name"),
            text("tenant"),
            FieldDefinition::builder("contacts", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("address", FieldType::Email).build(),
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![text("note")])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new("quote", vec![text("body")])])
                .build(),
        ];

        def
    }

    fn locales() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn people_tables(conn: &InMemoryConn) {
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE people (id TEXT PRIMARY KEY, email TEXT, work__en TEXT, work__de TEXT,
                   name TEXT, tenant TEXT, _deleted_at TEXT);
                 CREATE TABLE people_contacts (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER,
                   address TEXT, meta TEXT);
                 CREATE TABLE people_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER,
                   _block_type TEXT, data TEXT);",
            )
            .unwrap();
    }

    /// Column, per-locale, array-row and JSON-stored values are rewritten
    /// once; the gate stops a second pass.
    #[test]
    fn rewrites_every_stored_value_once() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute_batch(
                "INSERT INTO people (id, email, work__en, work__de, name)
                   VALUES ('p1', ' ANGE\u{300}LE@Example.com', 'J\u{dc}RGEN@Work.example',
                           'plain@work.example', 'Rene\u{301}');
                 INSERT INTO people_contacts (id, parent_id, _order, address, meta)
                   VALUES ('c1', 'p1', 0, 'Bob@Example.com', '{\"note\":\"Cafe\u{301}\"}');
                 INSERT INTO people_content (id, parent_id, _order, _block_type, data)
                   VALUES ('b1', 'p1', 0, 'quote', '{\"body\":\"Noe\u{308}l\"}');",
            )
            .unwrap();

        let registry = registry_with(people_def());
        canonicalize_if_needed(&conn, &registry, &locales()).unwrap();

        assert_eq!(
            stored(&conn, "SELECT email FROM people"),
            "ang\u{e8}le@example.com"
        );
        assert_eq!(
            stored(&conn, "SELECT work__en FROM people"),
            "j\u{fc}rgen@work.example"
        );
        assert_eq!(stored(&conn, "SELECT name FROM people"), "Ren\u{e9}");
        assert_eq!(
            stored(&conn, "SELECT address FROM people_contacts"),
            "bob@example.com"
        );
        assert_eq!(
            from_str::<Value>(&stored(&conn, "SELECT meta FROM people_contacts")).unwrap(),
            json!({ "note": "Caf\u{e9}" })
        );
        assert_eq!(
            from_str::<Value>(&stored(&conn, "SELECT data FROM people_content")).unwrap(),
            json!({ "body": "No\u{eb}l" })
        );

        conn.0
            .execute("UPDATE people SET email = 'Again@Example.com'", [])
            .unwrap();
        canonicalize_if_needed(&conn, &registry, &locales()).unwrap();
        assert_eq!(
            stored(&conn, "SELECT email FROM people"),
            "Again@Example.com",
            "the gate must stop a second pass"
        );
    }

    /// Two live documents whose unique addresses become one stop startup,
    /// naming both, and nothing is rewritten.
    #[test]
    fn colliding_unique_addresses_stop_startup() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute_batch(
                "INSERT INTO people (id, email) VALUES ('p1', 'ANG\u{c8}LE@example.com');
                 INSERT INTO people (id, email) VALUES ('p2', 'ange\u{300}le@example.com');",
            )
            .unwrap();

        let err = canonicalize_if_needed(&conn, &registry_with(people_def()), &locales())
            .unwrap_err()
            .to_string();

        assert!(err.contains("p1, p2"), "{err}");
        assert!(err.contains("ang\u{e8}le@example.com"), "{err}");
        assert_eq!(
            stored(&conn, "SELECT email FROM people WHERE id = 'p1'"),
            "ANG\u{c8}LE@example.com"
        );
    }

    /// A trashed document doesn't take part in uniqueness, so its duplicate
    /// address doesn't stop startup.
    #[test]
    fn a_trashed_duplicate_does_not_collide() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute_batch(
                "INSERT INTO people (id, email) VALUES ('p1', 'ANG\u{c8}LE@example.com');
                 INSERT INTO people (id, email, _deleted_at)
                   VALUES ('p2', 'ange\u{300}le@example.com', '2026-01-01T00:00:00Z');",
            )
            .unwrap();

        canonicalize_if_needed(&conn, &registry_with(people_def()), &locales()).unwrap();

        assert_eq!(
            stored(&conn, "SELECT email FROM people WHERE id = 'p1'"),
            "ang\u{e8}le@example.com"
        );
    }

    /// Text that differs only in how an accent was typed collides in a unique
    /// index spanning it, naming the index and the documents.
    #[test]
    fn values_colliding_in_a_unique_index_stop_startup() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute_batch(
                "INSERT INTO people (id, name, tenant) VALUES ('p1', 'Cafe\u{301}', 't1');
                 INSERT INTO people (id, name, tenant) VALUES ('p2', 'Caf\u{e9}', 't1');
                 INSERT INTO people (id, name, tenant) VALUES ('p3', 'Caf\u{e9}', 't2');",
            )
            .unwrap();

        let mut def = people_def();
        let mut index = IndexDefinition::new(vec!["name".to_string(), "tenant".to_string()]);
        index.unique = true;
        def.indexes = vec![index];

        let err = canonicalize_if_needed(&conn, &registry_with(def), &locales())
            .unwrap_err()
            .to_string();

        assert!(err.contains("(name, tenant)"), "{err}");
        assert!(err.contains("p1, p2") && !err.contains("p3"), "{err}");
    }

    /// A unique index spanning several fields has no trashed-row exemption, so
    /// a trashed document's value collides with a live one's.
    #[test]
    fn a_trashed_duplicate_collides_in_a_unique_index() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute_batch(
                "INSERT INTO people (id, name, tenant) VALUES ('p1', 'Cafe\u{301}', 't1');
                 INSERT INTO people (id, name, tenant, _deleted_at)
                   VALUES ('p2', 'Caf\u{e9}', 't1', '2026-01-01T00:00:00Z');",
            )
            .unwrap();

        let mut def = people_def();
        let mut index = IndexDefinition::new(vec!["name".to_string(), "tenant".to_string()]);
        index.unique = true;
        def.indexes = vec![index];

        let err = canonicalize_if_needed(&conn, &registry_with(def), &locales())
            .unwrap_err()
            .to_string();

        assert!(err.contains("p1, p2"), "{err}");
    }

    /// A localized field in a unique index is indexed by its default-locale
    /// column, so that column's values are the ones checked.
    #[test]
    fn a_localized_field_in_a_unique_index_is_checked_in_its_default_locale_column() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute_batch(
                "INSERT INTO people (id, work__en, tenant) VALUES ('p1', 'Bob@Example.com', 't1');
                 INSERT INTO people (id, work__en, tenant) VALUES ('p2', 'bob@example.com', 't1');",
            )
            .unwrap();

        let mut def = people_def();
        let mut index = IndexDefinition::new(vec!["work".to_string(), "tenant".to_string()]);
        index.unique = true;
        def.indexes = vec![index];

        let err = canonicalize_if_needed(&conn, &registry_with(def), &locales())
            .unwrap_err()
            .to_string();

        assert!(err.contains("p1, p2"), "{err}");
    }

    /// A locale code with a hyphen names its columns with an underscore
    /// (`work__pt_BR`); the rewrite reads and writes those columns instead of
    /// querying a `work__pt-BR` column that doesn't exist.
    #[test]
    fn a_hyphenated_locale_column_is_rewritten() {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_meta (key TEXT PRIMARY KEY, value TEXT);
                 CREATE TABLE people (id TEXT PRIMARY KEY, work__en TEXT, work__pt_BR TEXT,
                   _deleted_at TEXT);
                 INSERT INTO people (id, work__en, work__pt_BR)
                   VALUES ('p1', 'Bob@Example.com', 'Jo\u{c3}O@Example.com');",
            )
            .unwrap();

        let mut def = CollectionDefinition::new("people");
        def.fields = vec![
            FieldDefinition::builder("work", FieldType::Email)
                .localized(true)
                .build(),
        ];
        let locales = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "pt-BR".to_string()],
            fallback: true,
        };

        canonicalize_if_needed(&conn, &registry_with(def), &locales).unwrap();

        assert_eq!(
            stored(&conn, "SELECT work__pt_BR FROM people"),
            "jo\u{e3}o@example.com"
        );
    }

    /// Regression: the check read index values as text only, so a unique index
    /// spanning a number column saw every row as NULL and missed duplicates —
    /// startup then failed on the index with a raw constraint error.
    #[test]
    fn values_colliding_in_a_unique_index_with_a_number_column_stop_startup() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute_batch(
                "ALTER TABLE people ADD COLUMN rank REAL;
                 INSERT INTO people (id, name, rank) VALUES ('p1', 'Cafe\u{301}', 1);
                 INSERT INTO people (id, name, rank) VALUES ('p2', 'Caf\u{e9}', 1);",
            )
            .unwrap();

        let mut def = people_def();
        def.fields
            .push(FieldDefinition::builder("rank", FieldType::Number).build());
        let mut index = IndexDefinition::new(vec!["name".to_string(), "rank".to_string()]);
        index.unique = true;
        def.indexes = vec![index];

        let err = canonicalize_if_needed(&conn, &registry_with(def), &locales())
            .unwrap_err()
            .to_string();

        assert!(err.contains("p1, p2"), "{err}");
    }

    /// A text field added after a pass runs the rewrite again for its column.
    #[test]
    fn a_field_added_later_runs_the_rewrite_again() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        conn.0
            .execute(
                "INSERT INTO people (id, name) VALUES ('p1', 'Rene\u{301}')",
                [],
            )
            .unwrap();

        let mut without_name = people_def();
        without_name.fields.retain(|f| f.name != "name");
        canonicalize_if_needed(&conn, &registry_with(without_name), &locales()).unwrap();
        assert_eq!(stored(&conn, "SELECT name FROM people"), "Rene\u{301}");

        canonicalize_if_needed(&conn, &registry_with(people_def()), &locales()).unwrap();
        assert_eq!(stored(&conn, "SELECT name FROM people"), "Ren\u{e9}");
    }

    /// Regression: the rewrite read every row of a column in one SELECT, so a
    /// large table was materialised whole inside the migration transaction.
    /// The scan is paged — and still reaches the last row, in the main table,
    /// an array join table and a blocks join table alike.
    #[test]
    fn rewrites_every_row_past_the_first_page() {
        let conn = InMemoryConn::open();
        people_tables(&conn);
        let rows = PAGE_SIZE * 2 + 1;

        for i in 0..rows {
            let id = format!("r{i:05}");
            conn.0
                .execute(
                    "INSERT INTO people (id, name) VALUES (?1, 'Rene\u{301}')",
                    [&id],
                )
                .unwrap();
            conn.0
                .execute(
                    "INSERT INTO people_contacts VALUES (?1, 'p1', 0, 'Bob@Example.com', NULL)",
                    [&id],
                )
                .unwrap();
            conn.0
                .execute(
                    "INSERT INTO people_content VALUES (?1, 'p1', 0, 'quote', '{\"body\":\"Noe\u{308}l\"}')",
                    [&id],
                )
                .unwrap();
        }

        canonicalize_if_needed(&conn, &registry_with(people_def()), &locales()).unwrap();

        for (sql, expected) in [
            ("SELECT COUNT(*) FROM people WHERE name = 'Ren\u{e9}'", rows),
            (
                "SELECT COUNT(*) FROM people_contacts WHERE address = 'bob@example.com'",
                rows,
            ),
            (
                "SELECT COUNT(*) FROM people_content WHERE json_extract(data, '$.body') = 'No\u{eb}l'",
                rows,
            ),
        ] {
            let count: i64 = conn.0.query_row(sql, [], |r| r.get(0)).unwrap();
            assert_eq!(count, i64::try_from(expected).unwrap(), "{sql}");
        }
    }
}
