use crap_cms::config::LocaleConfig;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::CollectionDefinition;
use crap_cms::db::query::{find, FindQuery, LocaleContext, LocaleMode};
use rusqlite::Connection;

#[test]
fn test_sql_injection_locale_sanitization() {
    let conn = Connection::open_in_memory().unwrap();
    // Create a table with localized columns
    conn.execute_batch("CREATE TABLE posts (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT)")
        .unwrap();
    conn.execute(
        "INSERT INTO posts (id, title__en, title__de) VALUES ('1', 'Hello', 'Hallo')",
        [],
    )
    .unwrap();

    let mut def = CollectionDefinition::new("posts");
    def.timestamps = false;
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text)
        .localized(true)
        .build()];

    let config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: false,
    };

    // 1. Malicious locale with syntax-breaking characters
    let malicious_locale = "en) OR 1=1 --";
    let ctx = LocaleContext {
        mode: LocaleMode::Single(malicious_locale.to_string()),
        config: config.clone(),
    };

    let query = FindQuery::default();
    let result = find(&conn, "posts", &def, &query, Some(&ctx));

    // Should NOT error (it should fall back to default 'en') and NOT return extra data
    assert!(
        result.is_ok(),
        "Query should succeed despite malicious locale (due to fallback/sanitization)"
    );
    let docs = result.unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get_str("title"), Some("Hello"));

    // 2. Malicious locale with valid syntax (Union Select)
    let union_locale = "en AS title, (SELECT 'injected')";
    let ctx2 = LocaleContext {
        mode: LocaleMode::Single(union_locale.to_string()),
        config,
    };
    let result2 = find(&conn, "posts", &def, &query, Some(&ctx2));
    assert!(result2.is_ok());
    let docs2 = result2.unwrap();
    assert_eq!(docs2.len(), 1);
    // Should NOT return 'injected'
    assert_eq!(docs2[0].get_str("title"), Some("Hello"));
}

#[test]
fn test_sql_injection_via_union_in_locale() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE posts (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT)")
        .unwrap();
    conn.execute(
        "INSERT INTO posts (id, title__en, title__de) VALUES ('1', 'Post 1', 'Beitrag 1')",
        [],
    )
    .unwrap();

    // Create another table we want to steal data from
    conn.execute_batch("CREATE TABLE users (id TEXT PRIMARY KEY, password_hash TEXT)")
        .unwrap();
    conn.execute(
        "INSERT INTO users (id, password_hash) VALUES ('admin', 'SECRET_HASH')",
        [],
    )
    .unwrap();

    let mut def = CollectionDefinition::new("posts");
    def.timestamps = false;
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text)
        .localized(true)
        .build()];

    let config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string()],
        fallback: false,
    };

    // Attempt to UNION SELECT from users table
    let malicious_locale = "en FROM posts UNION SELECT id, password_hash FROM users --";

    let ctx = LocaleContext {
        mode: LocaleMode::Single(malicious_locale.to_string()),
        config,
    };

    let query = FindQuery::default();
    let result = find(&conn, "posts", &def, &query, Some(&ctx));

    assert!(result.is_ok());
    let docs = result.unwrap();

    // Should ONLY find the 1 document from 'posts', NOT the user data
    assert_eq!(docs.len(), 1, "Should not return injected union data");
    assert_eq!(docs[0].id, "1");
}
