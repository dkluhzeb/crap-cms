//! Postgres harness: full-text search — punctuated words, word-less terms,
//! the per-locale tsvectors and the per-write upsert that keeps them fresh.
//! The `SQLite` twins live beside the search builder (`db::query::fts`).

#![cfg(all(test, feature = "postgres"))]

use super::{pg_test_pool, support::drop_tables_matching, unique_slug};
use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, FieldDefinition, FieldType},
    db::{
        DbConnection, DbValue, LocaleContext, LocaleMode,
        query::fts::{
            FtsIndex, FtsSearch, fts_rank_order_by, fts_upsert, fts_where_clause, sync_fts_table,
        },
    },
};

fn en_de() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

fn def(slug: &str, localized_title: bool) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .localized(localized_title)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def
}

fn text(v: Option<&str>) -> DbValue {
    v.map_or(DbValue::Null, |v| DbValue::Text(v.to_string()))
}

/// Ids matching `term` through the search filter, in id order.
fn search_ids(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    term: &str,
    locale: Option<&LocaleContext>,
) -> Vec<String> {
    let search = FtsSearch::builder(&def.slug, def, term)
        .locale_ctx(locale)
        .build();
    let (clause, query) = fts_where_clause(conn, &search, 1)
        .unwrap()
        .expect("a search clause");

    conn.query_all(
        &format!("SELECT id FROM \"{}\" WHERE {clause} ORDER BY id", def.slug),
        &[DbValue::Text(query)],
    )
    .unwrap()
    .iter()
    .map(|row| row.get_string("id").unwrap())
    .collect()
}

/// Regression: the Postgres query stripped punctuation from every word, so an
/// email, a hyphenated word, a decimal or an apostrophe never matched the
/// token `to_tsvector` indexed — while the same search matched on `SQLite`.
/// The same corpus the `SQLite` test uses now matches identically.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn punctuated_words_match_like_sqlite() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let conn = pool.get().unwrap();
    let slug = unique_slug("fts_punct");
    let def = def(&slug, false);

    conn.execute_ddl(
        &format!("CREATE TABLE \"{slug}\" (id TEXT PRIMARY KEY, title TEXT, body TEXT)"),
        &[],
    )
    .unwrap();

    for (id, title, body) in [
        ("1", "jane@example.com", "well-known"),
        ("2", "pi is 3.14", "O'Brien"),
        ("3", "unrelated", "nothing"),
    ] {
        conn.execute(
            &format!("INSERT INTO \"{slug}\" (id, title, body) VALUES ($1, $2, $3)"),
            &[text(Some(id)), text(Some(title)), text(Some(body))],
        )
        .unwrap();
    }

    sync_fts_table(
        &conn,
        &FtsIndex::builder(&slug, &def, &LocaleConfig::default()).build(),
    )
    .unwrap();

    for (term, expected) in [
        ("jane@example.com", "1"),
        ("well-known", "1"),
        ("3.14", "2"),
        ("O'Brien", "2"),
        ("pi", "2"),
    ] {
        assert_eq!(
            search_ids(&conn, &def, term, None),
            vec![expected],
            "term {term}"
        );
    }

    // tsquery operators inside a word stay literal: no match, no syntax error.
    assert!(search_ids(&conn, &def, "a&b|!c:(d)'", None).is_empty());

    // A word-less term is no search on Postgres too — the same as `SQLite`.
    let search = FtsSearch::builder(&slug, &def, "---").build();
    assert!(fts_where_clause(&conn, &search, 1).unwrap().is_none());

    // The relevance sort reads the same tsvector and is valid SQL.
    let search = FtsSearch::builder(&slug, &def, "pi").build();
    let (order, query) = fts_rank_order_by(&conn, &search, 1).unwrap().unwrap();
    let rows = conn
        .query_all(
            &format!("SELECT id FROM \"{slug}\"{order}"),
            &[DbValue::Text(query)],
        )
        .unwrap();
    assert_eq!(rows[0].get_string("id").unwrap(), "2");

    drop_tables_matching(&conn, &slug);
}

/// A search in one locale matches that locale's text — through the fallback
/// while it holds nothing — and the non-localized fields; `locale = all`
/// matches every locale. The per-write upsert keeps every locale's tsvector
/// in step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_locale_search_matches_that_locales_text() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };
    let conn = pool.get().unwrap();
    let slug = unique_slug("fts_locale");
    let def = def(&slug, true);
    let config = en_de();

    conn.execute_ddl(
        &format!(
            "CREATE TABLE \"{slug}\" (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT, body TEXT)"
        ),
        &[],
    )
    .unwrap();

    let insert = |id: &str, en: Option<&str>, de: Option<&str>, body: &str| {
        conn.execute(
            &format!(
                "INSERT INTO \"{slug}\" (id, title__en, title__de, body) VALUES ($1, $2, $3, $4)"
            ),
            &[text(Some(id)), text(en), text(de), text(Some(body))],
        )
        .unwrap();
    };

    insert("both", Some("Hello"), Some("Hallo"), "shared");
    insert("en_only", Some("Orphan"), None, "lonely");

    let index = FtsIndex::builder(&slug, &def, &config).build();
    sync_fts_table(&conn, &index).unwrap();

    let in_locale = |mode: LocaleMode| LocaleContext {
        mode,
        config: config.clone(),
    };
    let de = in_locale(LocaleMode::Single("de".to_string()));
    let en = in_locale(LocaleMode::Default);
    let all = in_locale(LocaleMode::All);

    assert!(
        search_ids(&conn, &def, "Hello", Some(&de)).is_empty(),
        "en text, de search"
    );
    assert_eq!(search_ids(&conn, &def, "Hallo", Some(&de)), vec!["both"]);
    assert_eq!(search_ids(&conn, &def, "Hello", Some(&en)), vec!["both"]);
    assert!(
        search_ids(&conn, &def, "Hallo", Some(&en)).is_empty(),
        "de text, en search"
    );
    assert_eq!(search_ids(&conn, &def, "Hallo", Some(&all)), vec!["both"]);
    assert_eq!(
        search_ids(&conn, &def, "shared", Some(&de)),
        vec!["both"],
        "non-localized"
    );

    // A document with no German title shows (and is found by) its fallback.
    assert_eq!(
        search_ids(&conn, &def, "Orphan", Some(&de)),
        vec!["en_only"]
    );

    // Translating it re-indexes every locale's vector on the next write.
    conn.execute(
        &format!("UPDATE \"{slug}\" SET title__de = 'Waise' WHERE id = 'en_only'"),
        &[],
    )
    .unwrap();
    fts_upsert(&conn, &index, "en_only").unwrap();

    assert_eq!(search_ids(&conn, &def, "Waise", Some(&de)), vec!["en_only"]);
    assert!(search_ids(&conn, &def, "Orphan", Some(&de)).is_empty());
    assert_eq!(
        search_ids(&conn, &def, "Orphan", Some(&en)),
        vec!["en_only"]
    );

    drop_tables_matching(&conn, &slug);
}
