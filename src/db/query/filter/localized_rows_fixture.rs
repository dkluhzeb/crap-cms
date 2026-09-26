//! A localized has-many junction and a localized array whose documents hold
//! rows in some locales only — the fixture both backends' tests share to prove
//! a filter on localized join rows matches exactly the rows the read shows.

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{Document, FieldDefinition, FieldType, RelationshipConfig},
    db::{
        DbConnection, Filter, FilterClause, FilterOp, LocaleContext,
        query::{filter::build_where_clause, hydrate_document},
    },
};

/// The documents, sorted by id: `both` holds `en` and `de` rows, `en_only`
/// holds `en` rows alone, `none` holds no row.
const DOCS: [&str; 3] = ["both", "en_only", "none"];

fn fields() -> Vec<FieldDefinition> {
    vec![
        FieldDefinition::builder("tags", FieldType::Relationship)
            .localized(true)
            .relationship(RelationshipConfig::new("tags", true))
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
            ])
            .build(),
    ]
}

/// Create and fill the parent table `slug`, its `tags` junction and its
/// `items` array table. Plain literal SQL, so every backend runs it.
fn seed(conn: &dyn DbConnection, slug: &str) {
    let statements = [
        format!(
            "CREATE TABLE \"{slug}\" (id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0)"
        ),
        format!(
            "CREATE TABLE \"{slug}_tags\" \
             (parent_id TEXT, related_id TEXT, _order INTEGER, _locale TEXT)"
        ),
        format!(
            "CREATE TABLE \"{slug}_items\" \
             (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT, _locale TEXT)"
        ),
    ];

    for sql in statements {
        conn.execute_ddl(&sql, &[]).unwrap();
    }

    let inserts = [
        format!("INSERT INTO \"{slug}\" (id) VALUES ('both'), ('en_only'), ('none')"),
        format!(
            "INSERT INTO \"{slug}_tags\" (parent_id, related_id, _order, _locale) VALUES \
             ('both', 't1', 0, 'en'), ('both', 't2', 0, 'de'), ('en_only', 't1', 0, 'en')"
        ),
        format!(
            "INSERT INTO \"{slug}_items\" (id, parent_id, _order, label, _locale) VALUES \
             ('i1', 'both', 0, 'x', 'en'), ('i2', 'both', 0, 'y', 'de'), \
             ('i3', 'en_only', 0, 'x', 'en')"
        ),
    ];

    for sql in inserts {
        conn.execute(&sql, &[]).unwrap();
    }
}

/// The context a read under `locale` (a code or `"all"`) builds, over `en`
/// (default) and `de`.
fn locale_ctx(locale: &str, fallback: bool) -> LocaleContext {
    let config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback,
    };

    LocaleContext::from_locale_string(Some(locale), &config)
        .unwrap()
        .expect("localization is enabled")
}

/// The ids `field op` matches under `ctx`, sorted.
fn matching_ids(
    conn: &dyn DbConnection,
    slug: &str,
    field: &str,
    op: FilterOp,
    ctx: &LocaleContext,
) -> Vec<String> {
    let filters = vec![FilterClause::Single(Filter {
        field: field.to_string(),
        op,
    })];
    let mut params = Vec::new();
    let clause =
        build_where_clause(conn, &filters, slug, &fields(), Some(ctx), &mut params).unwrap();

    conn.query_all(
        &format!("SELECT id FROM \"{slug}\"{clause} ORDER BY id"),
        &params,
    )
    .unwrap_or_else(|e| panic!("{field} must execute: {e:#}"))
    .iter()
    .filter_map(|row| row.opt_text_at(0))
    .collect()
}

/// The `tags` ids and `items` labels the read shows for each document under
/// `ctx`, in [`DOCS`] order.
fn shown(
    conn: &dyn DbConnection,
    slug: &str,
    ctx: &LocaleContext,
) -> Vec<(Vec<String>, Vec<String>)> {
    let strings = |value: Option<&Value>, key: Option<&str>| -> Vec<String> {
        let rows = value.and_then(Value::as_array).cloned().unwrap_or_default();

        rows.iter()
            .filter_map(|row| key.map_or(Some(row), |k| row.get(k)))
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    };

    DOCS.iter()
        .map(|id| {
            let mut doc = Document::new((*id).to_string());
            hydrate_document(conn, slug, &fields(), &mut doc, None, Some(ctx)).unwrap();

            (
                strings(doc.fields.get("tags"), None),
                strings(doc.fields.get("items"), Some("label")),
            )
        })
        .collect()
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_string()).collect()
}

/// One locale reading: the context, the rows the read shows per document, and
/// the ids each filter must match — the documents whose shown rows satisfy it.
struct Reading {
    name: &'static str,
    ctx: LocaleContext,
    shown: Vec<(Vec<String>, Vec<String>)>,
    cases: Vec<(&'static str, FilterOp, Vec<String>)>,
}

fn readings() -> Vec<Reading> {
    let t1 = || FilterOp::Equals("t1".into());

    vec![
        // `de` with fallback: `en_only` shows its `en` rows.
        Reading {
            name: "de with fallback",
            ctx: locale_ctx("de", true),
            shown: vec![
                (ids(&["t2"]), ids(&["y"])),
                (ids(&["t1"]), ids(&["x"])),
                (vec![], vec![]),
            ],
            cases: vec![
                ("tags.id", t1(), ids(&["en_only"])),
                (
                    "tags.id",
                    FilterOp::NotEquals("t1".into()),
                    ids(&["both", "none"]),
                ),
                (
                    "tags.id",
                    FilterOp::NotIn(vec!["t1".into()]),
                    ids(&["both", "none"]),
                ),
                ("tags.id", FilterOp::Exists, ids(&["both", "en_only"])),
                ("tags.id", FilterOp::NotExists, ids(&["none"])),
                (
                    "items.label",
                    FilterOp::Equals("x".into()),
                    ids(&["en_only"]),
                ),
                ("items.label", FilterOp::Equals("y".into()), ids(&["both"])),
            ],
        },
        // `de` without fallback: `en_only` shows nothing.
        Reading {
            name: "de without fallback",
            ctx: locale_ctx("de", false),
            shown: vec![
                (ids(&["t2"]), ids(&["y"])),
                (vec![], vec![]),
                (vec![], vec![]),
            ],
            cases: vec![
                ("tags.id", t1(), ids(&[])),
                ("tags.id", FilterOp::NotEquals("t1".into()), ids(&DOCS)),
                ("tags.id", FilterOp::NotIn(vec!["t1".into()]), ids(&DOCS)),
                ("tags.id", FilterOp::Exists, ids(&["both"])),
                ("tags.id", FilterOp::NotExists, ids(&["en_only", "none"])),
                ("items.label", FilterOp::Equals("x".into()), ids(&[])),
            ],
        },
        // All locales: the read shows the default locale's rows.
        Reading {
            name: "all locales",
            ctx: locale_ctx("all", true),
            shown: vec![
                (ids(&["t1"]), ids(&["x"])),
                (ids(&["t1"]), ids(&["x"])),
                (vec![], vec![]),
            ],
            cases: vec![
                ("tags.id", t1(), ids(&["both", "en_only"])),
                ("tags.id", FilterOp::Equals("t2".into()), ids(&[])),
                ("tags.id", FilterOp::NotEquals("t1".into()), ids(&["none"])),
                ("tags.id", FilterOp::Exists, ids(&["both", "en_only"])),
                ("tags.id", FilterOp::NotExists, ids(&["none"])),
                ("items.label", FilterOp::Equals("y".into()), ids(&[])),
            ],
        },
    ]
}

/// Seed `slug` and assert, for every reading, that the read shows the expected
/// rows and every filter matches exactly the documents whose shown rows
/// satisfy it.
pub(crate) fn assert_filters_match_the_shown_rows(conn: &dyn DbConnection, slug: &str) {
    seed(conn, slug);

    for reading in readings() {
        assert_eq!(
            shown(conn, slug, &reading.ctx),
            reading.shown,
            "{}: shown rows",
            reading.name
        );

        for (field, op, expected) in reading.cases {
            let label = format!("{}: {field} {op:?}", reading.name);

            assert_eq!(
                matching_ids(conn, slug, field, op, &reading.ctx),
                expected,
                "{label}"
            );
        }
    }
}
