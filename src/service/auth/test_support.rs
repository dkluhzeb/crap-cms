//! Shared test fixtures for the auth submodule tests. Compiled
//! only under `#[cfg(test)]`; never reachable from production code.

#![cfg(all(test, feature = "sqlite"))]

use std::{sync::Arc, time::Duration};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

use crate::{
    core::{
        CollectionDefinition, FieldDefinition, FieldType,
        auth::{Argon2PasswordProvider, PasswordProvider},
        collection::Auth,
    },
    db::{DbConnection, DbPool, DbValue},
};

/// The `users` table every fixture here seeds.
const USERS_TABLE: &str = "CREATE TABLE users (
    id TEXT PRIMARY KEY,
    email TEXT UNIQUE,
    _password_hash TEXT,
    _locked INTEGER DEFAULT 0,
    _verified INTEGER DEFAULT 0,
    _session_version INTEGER DEFAULT 0,
    _reset_token TEXT,
    _reset_token_exp INTEGER,
    _verification_token TEXT,
    _verification_token_exp INTEGER,
    created_at TEXT,
    updated_at TEXT
)";

/// The auth collection definition matching [`USERS_TABLE`].
fn users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .unique(true)
            .build(),
    ];

    def
}

/// A pool of exactly ONE in-memory connection, seeded like [`setup`] with
/// `password` — a checkout while another is held times out quickly, so a test
/// can prove a flow releases its connection.
pub(super) fn single_connection_pool(password: &str) -> (DbPool, CollectionDefinition) {
    let pool = DbPool::from_pool(
        Pool::builder()
            .max_size(1)
            .connection_timeout(Duration::from_millis(250))
            .build(SqliteConnectionManager::memory())
            .expect("test pool"),
    );

    let hash = Argon2PasswordProvider.hash_password(password).unwrap();
    let conn = pool.get().expect("connection");

    conn.execute_batch(USERS_TABLE).unwrap();
    conn.execute(
        "INSERT INTO users (id, email, _verified, _password_hash) \
         VALUES ('u1', 'test@example.com', 1, ?1)",
        &[DbValue::Text(hash.as_ref().to_string())],
    )
    .unwrap();

    (pool, users_def())
}

/// Build an in-memory sqlite `users` table seeded with a single
/// verified `u1` user (email `test@example.com`, password
/// `secret123`). Returns the connection, the collection
/// definition, and the password provider so tests can construct a
/// `ServiceContext`.
pub(super) fn setup() -> (Connection, CollectionDefinition, Arc<dyn PasswordProvider>) {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(USERS_TABLE).unwrap();

    let def = users_def();

    let provider: Arc<dyn PasswordProvider> = Arc::new(Argon2PasswordProvider);

    conn.execute(
        "INSERT INTO users (id, email, _verified) VALUES ('u1', 'test@example.com', 1)",
        [],
    )
    .unwrap();

    let hash = provider.hash_password("secret123").unwrap();
    conn.execute(
        "UPDATE users SET _password_hash = ?1 WHERE id = 'u1'",
        [hash.as_ref()],
    )
    .unwrap();

    (conn, def, provider)
}
