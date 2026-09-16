//! What a change to the locale configuration does to already-stored content.
//!
//! Two things move under a schema sync when the locale configuration changes:
//!
//! * A field switched into (or out of) `localized` reads its values from
//!   another column from then on — [`ColumnPlan`] pairs each column a field
//!   stores with the column its values come from, and [`LocaleShape`] says
//!   when they have to move.
//! * The configured locales themselves change. [`warn_on_default_locale_change`]
//!   records the configuration's fingerprint and warns once when the default
//!   locale moves under content that is already stored.

use std::collections::HashSet;

use anyhow::{Context as _, Result};
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::Registry,
    db::{
        DbConnection,
        migrate::{
            helpers::{ColumnSpec, table_exists},
            meta,
        },
        query::helpers::{global_table, locale_column, quote_ident},
    },
};

/// The `_crap_meta` key holding the locale configuration the stored content
/// was last written under.
const LOCALE_FINGERPRINT_KEY: &str = "locale_config";

/// Leads a recorded [`LocaleShape`]; bump it to have every table's shape
/// recorded afresh — until it is, a table's columns carry on creation alone.
const SHAPE_VERSION: &str = "1";

/// The `_crap_meta` key recording which of a table's columns stored localized
/// values at the last schema sync.
fn shape_key(table: &str) -> String {
    format!("locale_shape:{table}")
}

/// One column a field stores, and the column a localization switch would move
/// its values from when this column is newly created.
pub(in crate::db::migrate) struct ColumnPlan {
    pub(in crate::db::migrate) name: String,
    carry_from: Option<String>,
}

impl ColumnPlan {
    fn new(name: String, carry_from: Option<String>) -> Self {
        Self { name, carry_from }
    }

    /// Move the values of the column this one replaces into it, when that
    /// column is actually present on the table.
    ///
    /// Marking an existing field `localized` adds `{field}__{default_locale}`
    /// beside the bare `{field}` the content sits in: every read then looks at
    /// the new column and the content is unreachable — and a schema cleanup
    /// would drop the bare column with it. Clearing `localized` (or turning
    /// localization off) is the mirror. [`LocaleShape::must_carry`] decides
    /// when to call this; it is written to be repeatable, so calling it again
    /// under an unchanged shape moves the same values again.
    ///
    /// Every row with a value moves, with no condition on what the target
    /// column holds: a column added with the field's DEFAULT (a checkbox, or
    /// any `default_value`) is backfilled with that default on both backends,
    /// so a target-is-empty condition would match no row at all and the
    /// content would stay behind. A row whose source holds nothing keeps the
    /// target as it is — that is the column's default, the same value a row
    /// written now would get, and it keeps a NOT NULL target valid.
    ///
    /// # Errors
    ///
    /// Returns an error if the UPDATE fails.
    pub(in crate::db::migrate) fn carry_values(
        &self,
        conn: &dyn DbConnection,
        table: &str,
        existing: &HashSet<String>,
    ) -> Result<()> {
        let Some(from) = self.carry_from.as_deref().filter(|c| existing.contains(*c)) else {
            return Ok(());
        };

        let (from, to) = (quote_ident(from), quote_ident(&self.name));
        let sql = format!(
            "UPDATE {} SET {to} = {from} WHERE {from} IS NOT NULL",
            quote_ident(table)
        );

        conn.execute(&sql, &[]).with_context(|| {
            format!(
                "Failed to carry '{}' into '{}' on '{table}'",
                self.carry_from.as_deref().unwrap_or_default(),
                self.name
            )
        })?;

        Ok(())
    }
}

/// Which of a table's columns hold localized values now, against the ones that
/// did at the last schema sync.
///
/// A carry cannot key on column creation alone. The column a flip moves the
/// values into is created once: flipping a field back finds it already there,
/// carries nothing, and the reads return whatever that column held before the
/// first flip — every edit made in between is lost to the reader while the
/// truth sits in a column the definition no longer names (and a schema cleanup
/// drops). Recording which columns were localized turns that into a decision
/// the shape makes: a flag that flipped moves its values whether or not this
/// sync created anything.
///
/// A table synced before a shape was recorded has none: its columns carry on
/// creation alone — what they did before — and the shape is recorded for the
/// flips that follow, so an upgrade never replays a flip that already happened.
pub(in crate::db::migrate) struct LocaleShape {
    previous: Option<HashSet<String>>,
    current: HashSet<String>,
}

impl LocaleShape {
    /// The shape recorded for `table`, against the one `specs` describe now.
    ///
    /// # Errors
    ///
    /// Returns an error if the meta read fails.
    pub(in crate::db::migrate) fn load(
        conn: &dyn DbConnection,
        table: &str,
        specs: &[ColumnSpec<'_>],
    ) -> Result<Self> {
        let current = specs
            .iter()
            .filter(|spec| spec.is_localized)
            .map(|spec| spec.col_name.clone())
            .collect();

        let stored = meta::get(conn, &shape_key(table))?;
        let previous = stored.as_deref().and_then(parse_shape);

        Ok(Self { previous, current })
    }

    /// Whether the values a field stores in `column` have to move into the
    /// column the reads take them from: this sync created that column, or the
    /// field's `localized` flag flipped since the shape was recorded.
    #[must_use]
    pub(in crate::db::migrate) fn must_carry(&self, column: &str, created: bool) -> bool {
        if created {
            return true;
        }

        let Some(previous) = self.previous.as_ref() else {
            return false;
        };

        previous.contains(column) != self.current.contains(column)
    }

    /// Record the shape this sync leaves behind, so the next one can tell a
    /// flipped flag from a column that has always been read where it is.
    ///
    /// # Errors
    ///
    /// Returns an error if the meta write fails.
    pub(in crate::db::migrate) fn record(
        &self,
        conn: &dyn DbConnection,
        table: &str,
    ) -> Result<()> {
        if self.previous.as_ref() == Some(&self.current) {
            return Ok(());
        }

        let mut columns: Vec<&str> = self.current.iter().map(String::as_str).collect();
        columns.sort_unstable();

        meta::upsert(
            conn,
            &shape_key(table),
            &format!("{SHAPE_VERSION}:{}", columns.join(",")),
        )
    }
}

/// The columns a recorded shape names — `None` for a value written under
/// another version, which names columns this one cannot read.
fn parse_shape(value: &str) -> Option<HashSet<String>> {
    let columns = value.strip_prefix(SHAPE_VERSION)?.strip_prefix(':')?;

    Some(
        columns
            .split(',')
            .filter(|column| !column.is_empty())
            .map(ToString::to_string)
            .collect(),
    )
}

/// The columns the stored column `base` expands to under `locale_config`,
/// each paired with the column a localization switch moves its values from.
///
/// A localized field stores one column per locale; only the DEFAULT locale's
/// carries the bare column's values, because that is where a non-localized
/// field's content was read from. A shared field stores the bare column and
/// takes the default locale's values back.
///
/// # Errors
///
/// Returns an error if a configured locale code has no column form.
pub(in crate::db::migrate) fn column_plans(
    base: &str,
    is_localized: bool,
    locale_config: &LocaleConfig,
) -> Result<Vec<ColumnPlan>> {
    if !is_localized {
        let default = locale_column(base, &locale_config.default_locale)?;

        return Ok(vec![ColumnPlan::new(base.to_string(), Some(default))]);
    }

    locale_config
        .locales
        .iter()
        .map(|locale| {
            let carry_from = (*locale == locale_config.default_locale).then(|| base.to_string());

            Ok(ColumnPlan::new(locale_column(base, locale)?, carry_from))
        })
        .collect()
}

/// Whether any collection or global holds a row — the condition under which a
/// default-locale change is visible to a reader.
fn has_stored_content(conn: &dyn DbConnection, registry: &Registry) -> Result<bool> {
    let tables = registry
        .collections
        .keys()
        .map(ToString::to_string)
        .chain(registry.globals.keys().map(|slug| global_table(slug)));

    for table in tables {
        if !table_exists(conn, &table)? {
            continue;
        }

        let sql = format!("SELECT 1 FROM {} LIMIT 1", quote_ident(&table));

        if conn.query_one(&sql, &[])?.is_some() {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Record the locale configuration the content is stored under, warning when
/// the default locale moved beneath existing content.
///
/// Changing `default_locale` silently re-points every default read at another
/// locale's columns: the content stays where it was written, so a reader sees
/// blanks (or the old default's values only through `fallback`) until the
/// translations are filled in. The configuration is the operator's to choose —
/// this warns and moves on rather than refusing to boot.
///
/// Call it once per schema sync, with the migration transaction, after the
/// tables are in place.
///
/// # Errors
///
/// Returns an error if the meta read/write or a table probe fails.
pub fn warn_on_default_locale_change(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let fingerprint = locale_config.fingerprint();
    let stored = meta::get(conn, LOCALE_FINGERPRINT_KEY)?;

    if stored.as_deref() == Some(fingerprint.as_str()) {
        return Ok(());
    }

    if let Some(previous_default) = stored
        .as_deref()
        .and_then(LocaleConfig::default_locale_of_fingerprint)
        .filter(|previous| *previous != locale_config.default_locale)
        && has_stored_content(conn, registry)?
    {
        warn!(
            "default_locale changed from '{}' to '{}' with content already stored: \
             existing values stay in the '{}' columns, default reads now take '{}', \
             fallback now resolves toward '{}', and completeness is judged against \
             '{}'. Translate or re-save the affected documents.",
            previous_default,
            locale_config.default_locale,
            previous_default,
            locale_config.default_locale,
            locale_config.default_locale,
            locale_config.default_locale,
        );
    }

    meta::upsert(conn, LOCALE_FINGERPRINT_KEY, &fingerprint)
}

#[cfg(test)]
mod tests {
    use std::slice;

    use super::*;
    use crate::{
        core::{FieldDefinition, FieldType},
        db::migrate::{
            collection::test_helpers::{in_memory_pool, locale_en_de, no_locale},
            helpers::collect_column_specs,
        },
    };

    fn plan_names(plans: &[ColumnPlan]) -> Vec<(&str, Option<&str>)> {
        plans
            .iter()
            .map(|p| (p.name.as_str(), p.carry_from.as_deref()))
            .collect()
    }

    /// Marking a field `localized` must carry the bare column's values into
    /// the DEFAULT locale's column — and only that one.
    #[test]
    fn a_localized_field_carries_the_bare_column_into_the_default_locale() {
        let plans = column_plans("title", true, &locale_en_de()).unwrap();

        assert_eq!(
            plan_names(&plans),
            vec![("title__en", Some("title")), ("title__de", None)]
        );
    }

    /// Clearing `localized` is the mirror: the bare column takes the default
    /// locale's values back.
    #[test]
    fn a_shared_field_carries_the_default_locale_column_back() {
        let plans = column_plans("title", false, &locale_en_de()).unwrap();

        assert_eq!(plan_names(&plans), vec![("title", Some("title__en"))]);
    }

    /// With localization off the bare column still takes back whatever a
    /// previous localized configuration left in the default locale's column.
    #[test]
    fn localization_off_still_reclaims_the_default_locale_column() {
        let plans = column_plans("title", false, &no_locale()).unwrap();

        assert_eq!(plan_names(&plans), vec![("title", Some("title__en"))]);
    }

    /// The column specs of one text field, localized or not.
    fn title_specs<'a>(
        field: &'a FieldDefinition,
        locale_config: &LocaleConfig,
    ) -> Vec<ColumnSpec<'a>> {
        collect_column_specs(slice::from_ref(field), locale_config)
    }

    fn title(localized: bool) -> FieldDefinition {
        FieldDefinition::builder("title", FieldType::Text)
            .localized(localized)
            .build()
    }

    /// A flag that flipped since the recorded shape moves its values even
    /// though nothing was created this sync — the column they move into was
    /// created by the flip before it, so keying on creation alone left the
    /// reads on the value the field held before that first flip.
    #[test]
    fn a_flipped_flag_carries_without_a_new_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let locale_config = locale_en_de();

        let localized = title(true);
        let first =
            LocaleShape::load(&conn, "posts", &title_specs(&localized, &locale_config)).unwrap();
        first.record(&conn, "posts").unwrap();

        let unchanged =
            LocaleShape::load(&conn, "posts", &title_specs(&localized, &locale_config)).unwrap();
        assert!(
            !unchanged.must_carry("title", false),
            "an unchanged flag moves nothing"
        );

        let shared = title(false);
        let flipped =
            LocaleShape::load(&conn, "posts", &title_specs(&shared, &locale_config)).unwrap();
        assert!(flipped.must_carry("title", false), "the flag flipped");
    }

    /// A table synced before a shape was recorded has none: its columns carry
    /// on creation alone, so an upgrade never replays a flip that already
    /// happened. The shape is recorded for the flips that follow.
    #[test]
    fn an_unrecorded_shape_carries_on_creation_alone() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let localized = title(true);

        let shape =
            LocaleShape::load(&conn, "posts", &title_specs(&localized, &locale_en_de())).unwrap();

        assert!(!shape.must_carry("title", false));
        assert!(shape.must_carry("title", true), "a created column carries");

        shape.record(&conn, "posts").unwrap();

        let recorded = meta::get(&conn, &shape_key("posts")).unwrap();
        assert_eq!(
            recorded.as_deref(),
            Some(format!("{SHAPE_VERSION}:title").as_str()),
            "the shape names the field's column, not its per-locale ones"
        );
    }

    fn columns(names: &[&str]) -> HashSet<String> {
        names.iter().map(|c| (*c).to_string()).collect()
    }

    /// The default locale's column takes the bare column's value; the other
    /// locales stay empty, and a second run changes nothing.
    #[test]
    fn switching_a_field_to_localized_keeps_its_stored_value() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, title__en TEXT, title__de TEXT);
             INSERT INTO posts (id, title) VALUES ('p1', 'Hello');",
        )
        .unwrap();

        let existing = columns(&["id", "title", "title__en", "title__de"]);
        let plans = column_plans("title", true, &locale_en_de()).unwrap();

        for plan in &plans {
            plan.carry_values(&conn, "posts", &existing).unwrap();
            plan.carry_values(&conn, "posts", &existing).unwrap();
        }

        let row = conn
            .query_one(
                "SELECT title__en, title__de FROM posts WHERE id = 'p1'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title__en").unwrap(), "Hello");
        assert!(row.opt_text_at(1).is_none(), "only the default locale");
    }

    /// Clearing `localized` moves the default locale's values back into the
    /// bare column the reads return to.
    #[test]
    fn switching_a_field_back_to_shared_reclaims_its_translation() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, title__en TEXT);
             INSERT INTO posts (id, title__en) VALUES ('p1', 'Hello');",
        )
        .unwrap();

        let existing = columns(&["id", "title", "title__en"]);

        for plan in &column_plans("title", false, &locale_en_de()).unwrap() {
            plan.carry_values(&conn, "posts", &existing).unwrap();
        }

        let row = conn
            .query_one("SELECT title FROM posts WHERE id = 'p1'", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_string("title").unwrap(), "Hello");
    }

    /// A source column the table does not have is skipped rather than named in
    /// an UPDATE that would fail.
    #[test]
    fn a_missing_source_column_is_skipped() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch("CREATE TABLE posts (id TEXT PRIMARY KEY, title__en TEXT);")
            .unwrap();

        for plan in &column_plans("title", true, &locale_en_de()).unwrap() {
            plan.carry_values(&conn, "posts", &columns(&["id", "title__en"]))
                .unwrap();
        }
    }

    /// The locale fingerprint is recorded on the first sync and re-checked on
    /// the next; only a changed `default_locale` is worth a warning, and the
    /// stored value always ends up matching the live configuration.
    #[test]
    fn a_changed_default_locale_is_detected_once() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let registry = Registry::default();

        let en_de = locale_en_de();
        warn_on_default_locale_change(&conn, &registry, &en_de).unwrap();
        assert_eq!(
            meta::get(&conn, LOCALE_FINGERPRINT_KEY).unwrap().as_deref(),
            Some(en_de.fingerprint().as_str())
        );

        let de_first = LocaleConfig {
            default_locale: "de".to_string(),
            ..en_de.clone()
        };
        assert_ne!(de_first.fingerprint(), en_de.fingerprint());

        warn_on_default_locale_change(&conn, &registry, &de_first).unwrap();
        assert_eq!(
            meta::get(&conn, LOCALE_FINGERPRINT_KEY).unwrap().as_deref(),
            Some(de_first.fingerprint().as_str()),
            "the recorded fingerprint follows the live configuration"
        );
    }

    /// Adding a locale changes the fingerprint (so the ref-count gate
    /// recomputes) without claiming the default locale moved.
    #[test]
    fn adding_a_locale_changes_the_fingerprint_but_not_the_default() {
        let en_de = locale_en_de();
        let mut en_de_fr = en_de.clone();
        en_de_fr.locales.push("fr".to_string());

        assert_ne!(en_de.fingerprint(), en_de_fr.fingerprint());
        assert_eq!(
            LocaleConfig::default_locale_of_fingerprint(&en_de_fr.fingerprint()),
            Some("en")
        );
    }
}
