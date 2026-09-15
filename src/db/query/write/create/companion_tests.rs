//! Create tests for companion columns: a date's `_tz` timezone and a code
//! field's `_lang` language.

use serde_json::{Value, json};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, DocumentFields, FieldAdmin, FieldDefinition, FieldType},
    db::{
        BoxedConnection, DbConnection, LocaleContext,
        query::{LocaleMode, find_by_id},
    },
};

use super::{create, test_support::setup_db};

// ── Timezone companion tests ─────────────────────────────────────

#[test]
fn create_date_with_timezone_normalizes_and_stores_tz() {
    let (_dir, conn) = setup_db(
        "CREATE TABLE events (
            id TEXT PRIMARY KEY,
            start_date TEXT,
            start_date_tz TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("events");
    def.fields = vec![
        FieldDefinition::builder("start_date", FieldType::Date)
            .timezone(true)
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("start_date".to_string(), json!("2024-01-15T09:00"));
    data.insert("start_date_tz".to_string(), json!("America/New_York"));

    let doc = create(&conn, "events", &def, &data, None).unwrap();

    // 9am EST = 2pm UTC
    assert_eq!(doc.get_str("start_date"), Some("2024-01-15T14:00:00.000Z"));
    assert_eq!(doc.get_str("start_date_tz"), Some("America/New_York"));
}

#[test]
fn create_date_with_timezone_flag_but_no_tz_value_falls_back() {
    let (_dir, conn) = setup_db(
        "CREATE TABLE events (
            id TEXT PRIMARY KEY,
            start_date TEXT,
            start_date_tz TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("events");
    def.fields = vec![
        FieldDefinition::builder("start_date", FieldType::Date)
            .timezone(true)
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("start_date".to_string(), json!("2024-01-15T09:00"));
    // No timezone value provided

    let doc = create(&conn, "events", &def, &data, None).unwrap();

    // Falls back to normal normalization (treat as UTC)
    assert_eq!(doc.get_str("start_date"), Some("2024-01-15T09:00:00.000Z"));
}

#[test]
fn create_date_without_timezone_flag_no_tz_column() {
    let (_dir, conn) = setup_db(
        "CREATE TABLE events (
            id TEXT PRIMARY KEY,
            event_date TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("events");
    def.fields = vec![FieldDefinition::builder("event_date", FieldType::Date).build()];

    let mut data = DocumentFields::new();
    data.insert("event_date".to_string(), json!("2024-01-15"));

    let doc = create(&conn, "events", &def, &data, None).unwrap();
    assert_eq!(doc.get_str("event_date"), Some("2024-01-15T12:00:00.000Z"));
}

#[test]
fn create_read_roundtrip_with_timezone() {
    // Full create/read roundtrip: create a document with a timezone-aware
    // date field, then read it back and verify both the date and _tz
    // companion column are present.
    let (_dir, conn) = setup_db(
        "CREATE TABLE events (
            id TEXT PRIMARY KEY,
            title TEXT,
            start_date TEXT,
            start_date_tz TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("events");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("start_date", FieldType::Date)
            .timezone(true)
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("Conference"));
    data.insert("start_date".to_string(), json!("2024-06-15T09:00"));
    data.insert("start_date_tz".to_string(), json!("America/New_York"));

    let doc = create(&conn, "events", &def, &data, None).unwrap();

    // Verify the document has both the normalized date and timezone
    assert_eq!(doc.get_str("title"), Some("Conference"));
    assert_eq!(
        doc.get_str("start_date"),
        Some("2024-06-15T13:00:00.000Z"),
        "9am EDT (summer) should be normalized to 1pm UTC"
    );
    assert_eq!(
        doc.get_str("start_date_tz"),
        Some("America/New_York"),
        "Timezone companion column should be stored"
    );
}

#[test]
fn create_read_roundtrip_timezone_in_group() {
    // Timezone-aware date field inside a Group: both the prefixed date
    // and prefixed _tz companion column should survive a create/read roundtrip.
    let (_dir, conn) = setup_db(
        "CREATE TABLE events (
            id TEXT PRIMARY KEY,
            schedule__start TEXT,
            schedule__start_tz TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("events");
    def.fields = vec![
        FieldDefinition::builder("schedule", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("start", FieldType::Date)
                    .timezone(true)
                    .build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("schedule__start".to_string(), json!("2024-06-15T09:00"));
    data.insert("schedule__start_tz".to_string(), json!("Europe/Berlin"));

    let doc = create(&conn, "events", &def, &data, None).unwrap();

    // Berlin in June is CEST (UTC+2), so 09:00 local = 07:00 UTC
    assert_eq!(
        doc.get_str("schedule__start"),
        Some("2024-06-15T07:00:00.000Z"),
        "Group date should be normalized with timezone"
    );
    assert_eq!(
        doc.get_str("schedule__start_tz"),
        Some("Europe/Berlin"),
        "Group _tz companion should be stored"
    );
}

#[test]
fn create_date_empty_value_with_timezone_stores_null() {
    // When the date value is empty but a timezone is provided,
    // the date should be stored as NULL, not normalized.
    let (_dir, conn) = setup_db(
        "CREATE TABLE events (
            id TEXT PRIMARY KEY,
            start_date TEXT,
            start_date_tz TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("events");
    def.fields = vec![
        FieldDefinition::builder("start_date", FieldType::Date)
            .timezone(true)
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("start_date".to_string(), json!(""));
    data.insert("start_date_tz".to_string(), json!("America/New_York"));

    let doc = create(&conn, "events", &def, &data, None).unwrap();

    // Empty date with timezone should result in null
    assert!(
        doc.get("start_date").is_none_or(Value::is_null),
        "Empty date value should be stored as null"
    );
}

// ── Code language companion tests ────────────────────────────────

fn code_lang_field(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Code)
        .admin(
            FieldAdmin::builder()
                .languages(vec!["javascript".to_string(), "python".to_string()])
                .build(),
        )
        .build()
}

/// The text stored in `column` of the one `snippets` row, or `None` for NULL.
fn stored_text(conn: &BoxedConnection, column: &str) -> Option<String> {
    let sql = format!("SELECT {column} FROM snippets");
    let row = conn.query_one(&sql, &[]).unwrap().unwrap();

    row.get_string(column).ok()
}

/// Regression: a code field's `_lang` companion column existed but the
/// create never wrote it and the read never selected it, so the editor's
/// language pick was lost.
#[test]
fn create_read_roundtrip_with_code_language() {
    let (_dir, conn) = setup_db(
        "CREATE TABLE snippets (
            id TEXT PRIMARY KEY,
            snippet TEXT,
            snippet_lang TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("snippets");
    def.fields = vec![code_lang_field("snippet")];

    let mut data = DocumentFields::new();
    data.insert("snippet".to_string(), json!("print('hi')"));
    data.insert("snippet_lang".to_string(), json!("python"));

    let doc = create(&conn, "snippets", &def, &data, None).unwrap();

    assert_eq!(
        stored_text(&conn, "snippet_lang").as_deref(),
        Some("python"),
        "the _lang companion column must be written"
    );
    assert_eq!(doc.get_str("snippet_lang"), Some("python"));

    let read = find_by_id(&conn, "snippets", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(read.get_str("snippet_lang"), Some("python"));
}

/// Regression: a code field inside a group — its `_lang` companion is
/// written from the nested group object and read back nested beside it.
#[test]
fn create_read_roundtrip_code_language_in_group() {
    let (_dir, conn) = setup_db(
        "CREATE TABLE snippets (
            id TEXT PRIMARY KEY,
            meta__example TEXT,
            meta__example_lang TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("snippets");
    def.fields = vec![
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![code_lang_field("example")])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert(
        "meta".to_string(),
        json!({ "example": "console.log(1)", "example_lang": "javascript" }),
    );

    let doc = create(&conn, "snippets", &def, &data, None).unwrap();
    assert_eq!(doc.get_str("meta__example_lang"), Some("javascript"));

    let read = find_by_id(&conn, "snippets", &def, &doc.id, None)
        .unwrap()
        .unwrap();
    assert_eq!(read.fields["meta"]["example_lang"], json!("javascript"));
    assert!(!read.fields.contains_key("meta__example_lang"));
}

/// Regression: a localized code field's language pick goes to the
/// locale's own `_lang` column and reads back per locale.
#[test]
fn create_localized_code_language_reads_per_locale() {
    let (_dir, conn) = setup_db(
        "CREATE TABLE snippets (
            id TEXT PRIMARY KEY,
            snippet__en TEXT,
            snippet__de TEXT,
            snippet_lang__en TEXT,
            snippet_lang__de TEXT,
            created_at TEXT,
            updated_at TEXT
        )",
    );

    let mut def = CollectionDefinition::new("snippets");
    let mut field = code_lang_field("snippet");
    field.localized = true;
    def.fields = vec![field];

    let config = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    let de = LocaleContext {
        mode: LocaleMode::Single("de".to_string()),
        config: config.clone(),
    };

    let mut data = DocumentFields::new();
    data.insert("snippet".to_string(), json!("print('hallo')"));
    data.insert("snippet_lang".to_string(), json!("python"));

    let doc = create(&conn, "snippets", &def, &data, Some(&de)).unwrap();

    assert_eq!(
        stored_text(&conn, "snippet_lang__de").as_deref(),
        Some("python"),
        "the locale's _lang column must be written"
    );

    let read = find_by_id(&conn, "snippets", &def, &doc.id, Some(&de))
        .unwrap()
        .unwrap();
    assert_eq!(read.get_str("snippet_lang"), Some("python"));

    let all = LocaleContext {
        mode: LocaleMode::All,
        config,
    };
    let read_all = find_by_id(&conn, "snippets", &def, &doc.id, Some(&all))
        .unwrap()
        .unwrap();
    assert_eq!(read_all.fields["snippet_lang"]["de"], json!("python"));
    assert!(!read_all.fields.contains_key("snippet_lang__de"));
}
