//! Postgres harness: two concurrent saves of different user settings both
//! land.

#![cfg(all(test, feature = "postgres"))]

use std::{thread, time::Duration};

use tokio::task::spawn_blocking;

use super::{pg_test_pool_sized, unique_slug};
use crate::{
    db::{DbConnection, DbValue, query},
    service::user_settings::{UserSettings, load_user_settings, update_user_settings},
};

/// Regression: a settings save read the whole blob unlocked and wrote it
/// back — "IMMEDIATE serializes" holds on `SQLite` only — so on Postgres a
/// locale save that read before a concurrent column save committed wrote the
/// old blob back and dropped the columns. The read now locks the row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_concurrent_settings_saves_both_land() {
    let Some(pool) = pg_test_pool_sized(4, None) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let user = unique_slug("settingsuser");

    pool.get()
        .expect("conn")
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS _crap_user_settings (
                user_id TEXT PRIMARY KEY,
                settings TEXT NOT NULL DEFAULT '{}'
            );",
        )
        .expect("settings table");

    // The column save holds the settings row while it edits the blob.
    let mut columns_conn = pool.write().expect("conn");
    let columns_tx = columns_conn.transaction().expect("tx");
    let stored = query::get_user_settings_locked(&columns_tx, &user).expect("locked read");

    let locale_save = {
        let (pool, user) = (pool.clone(), user.clone());
        spawn_blocking(move || update_user_settings(&pool, &user, |s| s.set_ui_locale("de")))
    };

    // Give the locale save time to reach the row before the column save commits.
    thread::sleep(Duration::from_millis(300));

    let mut settings = UserSettings::parse(Some(stored.as_str()));
    settings.set_columns("posts", &["title".to_string()]);
    query::set_user_settings(&columns_tx, &user, &settings.to_json()).expect("write");
    columns_tx.commit().expect("commit");

    locale_save.await.expect("task").expect("locale save");

    let conn = pool.get().expect("conn");
    let settings = load_user_settings(&conn, &user).expect("read");
    conn.execute(
        &format!(
            "DELETE FROM _crap_user_settings WHERE user_id = {}",
            conn.placeholder(1)
        ),
        &[DbValue::Text(user.clone())],
    )
    .expect("cleanup");

    assert_eq!(settings.ui_locale(), Some("de"), "the locale save landed");
    assert_eq!(
        settings.columns("posts"),
        Some(vec!["title".to_string()]),
        "the column save survived the locale save"
    );
}
