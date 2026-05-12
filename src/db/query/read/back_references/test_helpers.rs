//! Shared test fixtures for back_references/ submodules.

use crate::config::{CrapConfig, DatabaseConfig, LocaleConfig};
use crate::core::Registry;
use crate::core::{CollectionDefinition, GlobalDefinition};
use crate::db::{DbConnection, DbPool, DbValue, migrate, pool};

pub(super) fn no_locale() -> LocaleConfig {
    LocaleConfig::default()
}

pub(super) fn locale_en_de() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

pub(super) fn setup_db(
    collections: &[CollectionDefinition],
    globals: &[GlobalDefinition],
    locale: &LocaleConfig,
) -> (tempfile::TempDir, DbPool, Registry) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = CrapConfig {
        database: DatabaseConfig {
            path: "test.db".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

    let registry_shared = Registry::shared();
    {
        let mut reg = registry_shared.write().unwrap();
        for c in collections {
            reg.register_collection(c.clone());
        }
        for g in globals {
            reg.register_global(g.clone());
        }
    }
    migrate::sync_all(&db_pool, &registry_shared.read().unwrap(), locale).expect("sync");

    let registry = (*Registry::snapshot(&registry_shared)).clone();
    (tmp, db_pool, registry)
}

pub(super) fn insert_doc(conn: &dyn DbConnection, table: &str, id: &str) {
    conn.execute(
        &format!("INSERT INTO \"{}\" (id) VALUES (?1)", table),
        &[DbValue::Text(id.to_string())],
    )
    .unwrap();
}

pub(super) fn insert_doc_with_field(
    conn: &dyn DbConnection,
    table: &str,
    id: &str,
    col: &str,
    val: &str,
) {
    conn.execute(
        &format!(
            "INSERT INTO \"{}\" (id, \"{}\") VALUES (?1, ?2)",
            table, col
        ),
        &[
            DbValue::Text(id.to_string()),
            DbValue::Text(val.to_string()),
        ],
    )
    .unwrap();
}
