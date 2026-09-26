//! User settings service — per-user preferences (column visibility, locale, etc.).
//!
//! The settings are one JSON blob per user. [`UserSettings`] is the only
//! reader and writer of its shape:
//!
//! ```json
//! { "ui_locale": "de", "collections": { "posts": { "columns": ["title"] } } }
//! ```
//!
//! Per-collection preferences live under `collections`, so no collection slug
//! can collide with a user-level preference such as `ui_locale`. Blobs written
//! before that namespace kept each collection's preferences at the top level
//! (`{ "posts": { "columns": [...] } }`); they are still read, and the next
//! save of that collection moves its entry under `collections`.

use serde_json::{Map, Value, from_str, json};
use tracing::warn;

use anyhow::Context as _;

use crate::{
    core::default_label_locale,
    db::{DbConnection, DbPool, query},
    service::{ServiceError, commit_admitted},
};

/// Where per-collection list preferences live.
const COLLECTIONS_KEY: &str = "collections";

/// The user's admin UI locale preference.
const UI_LOCALE_KEY: &str = "ui_locale";

/// Top-level keys the blob itself owns. A collection may carry one of these
/// as its slug, so a top-level value under them is never a collection's
/// pre-namespace entry.
const RESERVED_KEYS: [&str; 2] = [COLLECTIONS_KEY, UI_LOCALE_KEY];

/// A user's parsed settings blob.
#[derive(Debug, Clone, PartialEq)]
pub struct UserSettings(Map<String, Value>);

impl UserSettings {
    /// Parse a stored settings blob. A missing, malformed, or non-object blob
    /// reads as empty settings.
    #[must_use]
    pub fn parse(json: Option<&str>) -> Self {
        let map = json
            .and_then(|s| from_str::<Value>(s).ok())
            .and_then(|v| match v {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();

        Self(map)
    }

    /// The preferred admin UI locale, if one is stored.
    #[must_use]
    pub fn ui_locale(&self) -> Option<&str> {
        self.0.get(UI_LOCALE_KEY).and_then(Value::as_str)
    }

    /// Store the preferred admin UI locale.
    pub fn set_ui_locale(&mut self, locale: &str) {
        self.0.insert(UI_LOCALE_KEY.to_string(), json!(locale));
    }

    /// The saved list columns for collection `slug`: the namespaced entry, or
    /// an entry written before the namespace existed.
    #[must_use]
    pub fn columns(&self, slug: &str) -> Option<Vec<String>> {
        let namespaced = self.0.get(COLLECTIONS_KEY).and_then(|c| c.get(slug));
        let entry = namespaced.or_else(|| self.legacy_entry(slug))?;

        let cols = entry.get("columns")?.as_array()?;

        Some(
            cols.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        )
    }

    /// Store the list columns for collection `slug` under the `collections`
    /// namespace, dropping the slug's pre-namespace entry. A top-level value
    /// that is not a preferences object (e.g. the `ui_locale` string) is not a
    /// collection entry and is left alone.
    pub fn set_columns(&mut self, slug: &str, columns: &[String]) {
        if self.legacy_entry(slug).is_some() {
            self.0.remove(slug);
        }

        let collections = self
            .0
            .entry(COLLECTIONS_KEY)
            .or_insert_with(|| Value::Object(Map::new()));

        if !collections.is_object() {
            *collections = Value::Object(Map::new());
        }

        collections[slug] = json!({ "columns": columns });
    }

    /// The pre-namespace preferences object of collection `slug`, if there is
    /// one. A reserved key is the blob's own and never a collection entry,
    /// whatever the collection is called.
    fn legacy_entry(&self, slug: &str) -> Option<&Value> {
        if RESERVED_KEYS.contains(&slug) {
            return None;
        }

        self.0.get(slug).filter(|v| v.is_object())
    }

    /// Serialize for storage.
    #[must_use]
    pub fn to_json(&self) -> String {
        Value::Object(self.0.clone()).to_string()
    }
}

/// Get a user's settings JSON string, or None if not set.
pub fn get_user_settings(
    conn: &dyn DbConnection,
    user_id: &str,
) -> Result<Option<String>, ServiceError> {
    Ok(query::get_user_settings(conn, user_id)?)
}

/// Load and parse a user's settings (empty when none are stored).
///
/// # Errors
///
/// Returns the backend error when the settings row cannot be read.
pub fn load_user_settings(
    conn: &dyn DbConnection,
    user_id: &str,
) -> Result<UserSettings, ServiceError> {
    let stored = get_user_settings(conn, user_id)?;

    Ok(UserSettings::parse(stored.as_deref()))
}

/// The admin UI locale to address `user_id` in outside a request (a system
/// email): the user's own preference, else the configured default locale.
/// A settings row that cannot be read falls back to the default — the
/// message still goes out, in the default language.
pub fn recipient_ui_locale(conn: &dyn DbConnection, user_id: &str) -> String {
    let settings = load_user_settings(conn, user_id)
        .inspect_err(|e| warn!("Cannot read the UI locale of user {user_id}: {e:#}"))
        .ok();

    settings
        .and_then(|s| s.ui_locale().map(str::to_string))
        .unwrap_or_else(|| default_label_locale().to_string())
}

/// Change a user's settings: read the stored blob under a row lock, apply
/// `edit`, write it back — in one write transaction. The one read-modify-write
/// of the blob, so two concurrent saves of different settings (a column
/// preference and the UI locale, say) both land instead of the later one
/// writing back the blob it read before the other committed.
///
/// # Errors
///
/// Returns the backend error when no write connection is available or the
/// read, write or commit fails.
pub fn update_user_settings(
    pool: &DbPool,
    user_id: &str,
    edit: impl FnOnce(&mut UserSettings),
) -> Result<(), ServiceError> {
    let mut conn = pool.write().context("Failed to get DB connection")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start settings transaction")?;

    let stored = query::get_user_settings_locked(&tx, user_id)?;
    let mut settings = UserSettings::parse(Some(stored.as_str()));

    edit(&mut settings);

    set_user_settings(&tx, user_id, &settings.to_json())?;

    // A request already answered as timed out must not change anything.
    commit_admitted(tx)
}

/// Save a user's settings JSON string (upsert).
pub fn set_user_settings(
    conn: &dyn DbConnection,
    user_id: &str,
    settings_json: &str,
) -> Result<(), ServiceError> {
    query::set_user_settings(conn, user_id, settings_json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "sqlite")]
    use tempfile::TempDir;

    #[cfg(feature = "sqlite")]
    use crate::{
        config::CrapConfig,
        db::{InMemoryConn, pool::create_pool},
    };

    use super::*;

    /// An in-memory database holding the settings table, with `settings`
    /// stored for user `u1` when given.
    #[cfg(feature = "sqlite")]
    fn settings_db(settings: Option<&str>) -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.0
            .execute_batch(
                "CREATE TABLE _crap_user_settings (user_id TEXT PRIMARY KEY, settings TEXT)",
            )
            .expect("settings table");

        if let Some(blob) = settings {
            set_user_settings(&conn, "u1", blob).expect("store settings");
        }

        conn
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn recipient_locale_is_the_users_ui_preference() {
        let conn = settings_db(Some(r#"{"ui_locale":"de"}"#));

        assert_eq!(recipient_ui_locale(&conn, "u1"), "de");
    }

    #[cfg(feature = "sqlite")]
    #[test]
    fn recipient_locale_falls_back_to_the_default_locale() {
        let conn = settings_db(None);
        assert_eq!(recipient_ui_locale(&conn, "u1"), default_label_locale());

        let unreadable = InMemoryConn::open();
        assert_eq!(
            recipient_ui_locale(&unreadable, "u1"),
            default_label_locale()
        );
    }

    /// Each save changes only its own setting: the column preference and the
    /// UI locale saved one after the other both survive.
    #[cfg(feature = "sqlite")]
    #[test]
    fn update_user_settings_keeps_the_other_settings() {
        let dir = TempDir::new().unwrap();
        let pool = create_pool(dir.path(), &CrapConfig::default()).unwrap();
        pool.write()
            .unwrap()
            .execute_batch(
                "CREATE TABLE _crap_user_settings (user_id TEXT PRIMARY KEY, \
                 settings TEXT NOT NULL DEFAULT '{}')",
            )
            .unwrap();

        update_user_settings(&pool, "u1", |s| s.set_columns("posts", &cols(&["title"]))).unwrap();
        update_user_settings(&pool, "u1", |s| s.set_ui_locale("de")).unwrap();

        let settings = load_user_settings(&pool.get().unwrap(), "u1").unwrap();
        assert_eq!(settings.columns("posts"), Some(cols(&["title"])));
        assert_eq!(settings.ui_locale(), Some("de"));
    }

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn missing_or_malformed_blobs_read_as_empty() {
        assert_eq!(UserSettings::parse(None).to_json(), "{}");
        assert_eq!(UserSettings::parse(Some("not json")).to_json(), "{}");
        assert_eq!(UserSettings::parse(Some("[1]")).to_json(), "{}");
    }

    /// Regression: column preferences were stored at the top level keyed by
    /// the collection slug, so a collection named `ui_locale` overwrote the
    /// UI-locale preference (and vice versa). They live under `collections`.
    #[test]
    fn a_collection_named_ui_locale_does_not_clobber_the_locale() {
        let mut settings = UserSettings::parse(None);
        settings.set_ui_locale("de");
        settings.set_columns("ui_locale", &cols(&["title"]));

        let reloaded = UserSettings::parse(Some(&settings.to_json()));
        assert_eq!(reloaded.ui_locale(), Some("de"));
        assert_eq!(reloaded.columns("ui_locale"), Some(cols(&["title"])));

        let mut settings = reloaded;
        settings.set_ui_locale("en");
        assert_eq!(settings.columns("ui_locale"), Some(cols(&["title"])));
    }

    /// Settings saved before the namespace keep working, and the next save of
    /// that collection migrates its entry under `collections`.
    #[test]
    fn pre_namespace_columns_are_read_and_migrated_on_save() {
        let legacy = r#"{"ui_locale":"de","posts":{"columns":["title","views"]}}"#;
        let mut settings = UserSettings::parse(Some(legacy));

        assert_eq!(settings.columns("posts"), Some(cols(&["title", "views"])));
        assert_eq!(settings.ui_locale(), Some("de"));

        settings.set_columns("posts", &cols(&["views"]));
        let stored: Value = from_str(&settings.to_json()).unwrap();

        assert!(
            stored.get("posts").is_none(),
            "legacy entry moved: {stored}"
        );
        assert_eq!(stored["collections"]["posts"]["columns"], json!(["views"]));
        assert_eq!(stored["ui_locale"], "de");
    }

    /// The namespaced entry wins over a stale pre-namespace one.
    /// Regression: the pre-namespace migration treated the top-level
    /// `collections` value as the entry of a collection named `collections`,
    /// so saving that collection's columns deleted every other collection's.
    #[test]
    fn a_collection_named_collections_keeps_the_other_collections() {
        let mut settings = UserSettings::parse(None);
        settings.set_columns("posts", &["title".to_string()]);
        settings.set_columns("collections", &["name".to_string()]);

        assert_eq!(settings.columns("posts"), Some(vec!["title".to_string()]));
        assert_eq!(
            settings.columns("collections"),
            Some(vec!["name".to_string()])
        );
    }

    #[test]
    fn namespaced_columns_win_over_legacy() {
        let blob = r#"{"posts":{"columns":["a"]},"collections":{"posts":{"columns":["b"]}}}"#;
        assert_eq!(
            UserSettings::parse(Some(blob)).columns("posts"),
            Some(cols(&["b"]))
        );
    }

    #[test]
    fn a_locale_string_is_never_read_as_columns() {
        let settings = UserSettings::parse(Some(r#"{"ui_locale":"de"}"#));
        assert_eq!(settings.columns("ui_locale"), None);
    }
}
