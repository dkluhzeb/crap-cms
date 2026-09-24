//! Scanning the database for leftovers no Lua definition accounts for.

use std::collections::HashSet;

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, Registry},
    db::{
        DbConnection,
        migrate::{self, OrphanTable},
        query::{
            self, get_expected_junction_columns,
            helpers::{global_table, join_table},
            join_fields,
        },
    },
};

/// What a scan found: orphan columns per table, junction rows left behind by a
/// locale the project no longer configures, and whole tables no definition
/// accounts for.
///
/// Every field is required and the struct is built in one place, so it is
/// constructed as a plain literal.
pub(super) struct CleanupReport {
    pub(super) columns: Vec<(String, Vec<String>)>,
    pub(super) stale_locale_rows: Vec<(String, i64)>,
    pub(super) tables: Vec<OrphanTable>,
}

impl CleanupReport {
    pub(super) fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.stale_locale_rows.is_empty() && self.tables.is_empty()
    }
}

/// Scan for everything no Lua definition accounts for.
pub(super) fn scan(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<CleanupReport> {
    Ok(CleanupReport {
        columns: find_orphan_columns(conn, registry, locale_config)?,
        stale_locale_rows: find_stale_locale_rows(conn, registry, locale_config)?,
        tables: migrate::find_orphan_tables(conn, registry)?,
    })
}

/// Every table the scan inspects, paired with the columns its definition
/// expects: one entry per collection table, one per global table, and one per
/// junction table either of them owns.
///
/// Globals are included for the same reason collections are — a removed field
/// leaves `_global_site.title__fr` behind exactly as it would on a collection,
/// and a scan that skipped them reported the schema as clean. Junction tables
/// for the same reason again: a removed array sub-field leaves
/// `posts_items.label` behind.
fn expected_columns_by_table(
    reg: &Registry,
    locale_config: &LocaleConfig,
) -> Result<Vec<(String, HashSet<String>)>> {
    let mut tables = Vec::new();

    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        let def = &reg.collections[slug];
        tables.push((
            slug.to_string(),
            query::get_expected_column_names(def, locale_config)?,
        ));
        tables.extend(expected_junction_columns(slug, &def.fields));
    }

    let mut global_slugs: Vec<_> = reg.globals.keys().collect();
    global_slugs.sort();

    for slug in global_slugs {
        let def = &reg.globals[slug];
        let table = global_table(slug);
        tables.extend(expected_junction_columns(&table, &def.fields));
        tables.push((
            table,
            query::get_expected_global_column_names(def, locale_config)?,
        ));
    }

    Ok(tables)
}

/// The junction tables `fields` imply off `owner`, each paired with the
/// columns its owning field expects.
fn expected_junction_columns(
    owner: &str,
    fields: &[FieldDefinition],
) -> Vec<(String, HashSet<String>)> {
    let mut tables = Vec::new();

    for (name, field) in join_fields(fields) {
        tables.push((
            join_table(owner, &name),
            get_expected_junction_columns(field),
        ));
    }

    tables
}

/// Find orphan columns across all collection and global tables.
///
/// Returns a vec of (`table_name`, `vec_of_orphan_column_names`).
/// System columns (`_`-prefixed, `id`, `created_at`, `updated_at`) are excluded.
/// Plugin columns are NOT orphans because plugins run during `init_lua` and their
/// fields are included in the registry definitions.
///
/// # Errors
///
/// Returns an error if schema inspection or column-name expansion fails.
pub(super) fn find_orphan_columns(
    conn: &dyn DbConnection,
    reg: &Registry,
    locale_config: &LocaleConfig,
) -> Result<Vec<(String, Vec<String>)>> {
    let mut results = Vec::new();

    for (table, expected) in expected_columns_by_table(reg, locale_config)? {
        let existing = migrate::helpers::get_table_columns(conn, &table)?;

        if existing.is_empty() {
            continue;
        }

        let mut orphan_cols: Vec<String> = existing
            .iter()
            .filter(|col| !expected.contains(*col) && !col.starts_with('_'))
            .cloned()
            .collect();

        if !orphan_cols.is_empty() {
            orphan_cols.sort();
            results.push((table, orphan_cols));
        }
    }

    Ok(results)
}

/// Every junction table the registry implies, for collections and globals
/// alike. A table that was never created (a has-one relationship, a definition
/// that changed) is filtered out by the caller's existence check.
fn junction_tables(reg: &Registry) -> Vec<String> {
    let mut tables = Vec::new();

    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        for field in query::join_field_names(&reg.collections[slug].fields) {
            tables.push(join_table(slug, &field));
        }
    }

    let mut global_slugs: Vec<_> = reg.globals.keys().collect();
    global_slugs.sort();

    for slug in global_slugs {
        let owner = global_table(slug);
        for field in query::join_field_names(&reg.globals[slug].fields) {
            tables.push(join_table(&owner, &field));
        }
    }

    tables
}

/// Find junction rows whose `_locale` names a locale the project no longer
/// configures.
///
/// Nothing is reported while localization is off: an empty locale list means
/// "not localized", not "every stored row is stale", and a table that still
/// carries a `_locale` column from a time when localization was on must keep
/// its rows.
///
/// # Errors
///
/// Returns an error if schema inspection or the row count fails.
pub(super) fn find_stale_locale_rows(
    conn: &dyn DbConnection,
    reg: &Registry,
    locale_config: &LocaleConfig,
) -> Result<Vec<(String, i64)>> {
    if !locale_config.is_enabled() {
        return Ok(Vec::new());
    }

    let mut results = Vec::new();

    for table in junction_tables(reg) {
        let columns = migrate::helpers::get_table_columns(conn, &table)?;

        if !columns.contains("_locale") {
            continue;
        }

        let count = query::count_rows_outside_locales(conn, &table, &locale_config.locales)?;

        if count > 0 {
            results.push((table, count));
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        commands::db::cleanup::test_support::{
            locale_en_de, localized_array, make_conn, no_locale, simple_collection, text_field,
        },
        core::{FieldAdmin, FieldTab, FieldType, GlobalDefinition},
    };

    #[test]
    fn no_orphans_when_columns_match() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection("posts", vec![text_field("title")])),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn detects_orphan_column() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, old_field TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection("posts", vec![text_field("title")])),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].0, "posts");
        assert_eq!(orphans[0].1, vec!["old_field"]);
    }

    #[test]
    fn system_columns_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE users (id TEXT, email TEXT, _password_hash TEXT, _locked INTEGER, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "users".into(),
            Arc::new(simple_collection("users", vec![text_field("email")])),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty(), "system columns should not be flagged");
    }

    #[test]
    fn group_fields_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, seo__meta_title TEXT, seo__meta_desc TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![text_field("meta_title"), text_field("meta_desc")])
                        .build(),
                ],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty(), "group fields should not be flagged");
    }

    #[test]
    fn localized_columns_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title__en TEXT, title__de TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .localized(true)
                        .build(),
                ],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &locale_en_de()).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn detects_orphan_among_valid_columns() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, removed_field TEXT, seo__meta TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection(
                "posts",
                vec![
                    text_field("title"),
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![text_field("meta")])
                        .build(),
                ],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].1, vec!["removed_field"]);
    }

    #[test]
    fn nested_group_in_row_in_tabs_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, seo__title TEXT, body TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("layout", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Content",
                            vec![
                                FieldDefinition::builder("row", FieldType::Row)
                                    .fields(vec![
                                        FieldDefinition::builder("seo", FieldType::Group)
                                            .fields(vec![text_field("title")])
                                            .build(),
                                        text_field("body"),
                                    ])
                                    .build(),
                            ],
                        )])
                        .build(),
                ],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(
            orphans.is_empty(),
            "nested Group→Row→Tabs columns should not be orphans: {orphans:?}"
        );
    }

    fn simple_global(slug: &str, fields: Vec<FieldDefinition>) -> GlobalDefinition {
        let mut def = GlobalDefinition::new(slug);
        def.fields = fields;

        def
    }

    /// Regression: the scan iterated collections only, so a removed field left a
    /// column behind in a global's table — `_global_site.title__fr` — and the
    /// command reported the schema as clean.
    #[test]
    fn detects_orphan_column_in_a_global() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE _global_site (id TEXT, title TEXT, retired TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.globals.insert(
            "site".into(),
            Arc::new(simple_global("site", vec![text_field("title")])),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].0, "_global_site");
        assert_eq!(orphans[0].1, vec!["retired"]);
    }

    /// A global's declared columns — including its per-locale ones — are not
    /// orphans, and neither are the timestamps its table always carries.
    #[test]
    fn a_globals_declared_columns_are_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE _global_site (id TEXT, title__en TEXT, title__de TEXT, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.globals.insert(
            "site".into(),
            Arc::new(simple_global(
                "site",
                vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .localized(true)
                        .build(),
                ],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &locale_en_de()).unwrap();
        assert!(orphans.is_empty(), "{orphans:?}");
    }

    /// A global's junction table is scanned the same way a collection's is.
    #[test]
    fn detects_junction_rows_of_a_global() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE _global_site (id TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE _global_site_links (id TEXT PRIMARY KEY, parent_id TEXT, _locale TEXT, label TEXT);
             INSERT INTO _global_site_links VALUES ('a', 'default', 'fr', 'stale');",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.globals.insert(
            "site".into(),
            Arc::new(simple_global("site", vec![localized_array("links")])),
        );

        let stale = find_stale_locale_rows(&conn, &reg, &locale_en_de()).unwrap();
        assert_eq!(stale, vec![("_global_site_links".to_string(), 1)]);
    }

    /// With localization off there are no configured locales to compare
    /// against — a table still carrying `_locale` keeps every row rather than
    /// having all of them read as stale.
    #[test]
    fn localization_off_reports_no_stale_rows() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _locale TEXT, label TEXT);
             INSERT INTO posts_items VALUES ('a', 'p1', 'fr', 'kept');",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection("posts", vec![localized_array("items")])),
        );

        assert!(
            find_stale_locale_rows(&conn, &reg, &no_locale())
                .unwrap()
                .is_empty()
        );
    }

    /// A junction table with no `_locale` column (a non-localized array) is
    /// never scanned, and a table that was never created is skipped.
    #[test]
    fn a_non_localized_junction_table_is_not_scanned() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, label TEXT);
             INSERT INTO posts_items VALUES ('a', 'p1', 'kept');",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("items", FieldType::Array)
                        .fields(vec![text_field("label")])
                        .build(),
                    FieldDefinition::builder("gone", FieldType::Blocks).build(),
                ],
            )),
        );

        assert!(
            find_stale_locale_rows(&conn, &reg, &locale_en_de())
                .unwrap()
                .is_empty()
        );
    }

    fn code_lang_field(name: &str, localized: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .localized(localized)
            .build()
    }

    /// Regression: a code field's `_lang` companion columns were missing from
    /// the expected column set, so cleanup reported them as orphans and
    /// `--confirm` dropped every stored language pick.
    #[test]
    fn code_language_companion_columns_not_orphans() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE snippets (id TEXT, snippet TEXT, snippet_lang TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE notes (id TEXT, body__en TEXT, body__de TEXT, body_lang__en TEXT, body_lang__de TEXT, created_at TEXT, updated_at TEXT);",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "snippets".into(),
            Arc::new(simple_collection(
                "snippets",
                vec![code_lang_field("snippet", false)],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();
        assert!(orphans.is_empty(), "shared _lang column: {orphans:?}");

        reg.collections.insert(
            "notes".into(),
            Arc::new(simple_collection(
                "notes",
                vec![code_lang_field("body", true)],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &locale_en_de()).unwrap();
        assert!(orphans.is_empty(), "per-locale _lang columns: {orphans:?}");
    }

    /// Regression: the scan walked collection and global tables only, so a
    /// sub-field removed from an array left a column in the junction table
    /// that nothing ever reported. The live sub-fields and the junction's own
    /// system columns must stay out of the report.
    #[test]
    fn detects_orphan_column_in_a_junction_table() {
        let (_dir, conn) = make_conn();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT, title TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts_items (
                id TEXT, parent_id TEXT, _order INTEGER, label TEXT, retired TEXT
             );",
        )
        .unwrap();

        let mut reg = Registry::default();
        reg.collections.insert(
            "posts".into(),
            Arc::new(simple_collection(
                "posts",
                vec![
                    text_field("title"),
                    FieldDefinition::builder("items", FieldType::Array)
                        .fields(vec![text_field("label")])
                        .build(),
                ],
            )),
        );

        let orphans = find_orphan_columns(&conn, &reg, &no_locale()).unwrap();

        assert_eq!(orphans.len(), 1, "{orphans:?}");
        assert_eq!(orphans[0].0, "posts_items");
        assert_eq!(orphans[0].1, vec!["retired"]);
    }
}
