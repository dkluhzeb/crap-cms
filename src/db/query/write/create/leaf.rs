//! INSERT params for a single leaf (scalar) field and its companions.

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{DocumentFields, FieldDefinition, FieldType},
    db::{
        DbConnection, DbValue, LocaleContext,
        query::{
            helpers::{column_value, companion_writes, prefixed_name, tz_column},
            locale_write_column,
            write::create::collector::InsertCollector,
        },
    },
};

/// Collect INSERT params for a single leaf (scalar) field.
pub(super) fn collect_leaf_param(
    field: &FieldDefinition,
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
    collector: &mut InsertCollector,
    conn: &dyn DbConnection,
    prefix: &str,
    inherited_localized: bool,
) -> Result<()> {
    let data_key = prefixed_name(prefix, &field.name);
    let col_name = locale_write_column(&data_key, field, locale_ctx, inherited_localized)?;

    for (companion, companion_value) in companion_writes(field, &data_key, data) {
        let column = locale_write_column(&companion, field, locale_ctx, inherited_localized)?;
        collector.push(conn, &column, companion_value);
    }

    let Some(value) = data.get(&data_key) else {
        // A field the write doesn't carry takes its configured default here,
        // through the same encoder a sent value goes through. The column
        // `DEFAULT` can only be written when the column is created, so leaving
        // the job to it would freeze the default a table was born with; doing
        // it app-side makes a changed default take effect on the next create
        // without a migration.
        if let Some(default) = field.default_value.as_ref() {
            collector.push(conn, &col_name, column_value(field, default, None));
        } else if field.field_type == FieldType::Checkbox {
            // A checkbox without a default is off, not NULL: the read decodes
            // the column as a bool and a NULL would surface as one anyway.
            collector.push(conn, &col_name, DbValue::Integer(0));
        }

        return Ok(());
    };

    let zone = data.get(&tz_column(&data_key)).and_then(Value::as_str);
    collector.push(conn, &col_name, column_value(field, value, zone));

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldType},
        db::query::write::create::{
            create,
            test_support::{setup_db, test_def},
        },
    };

    /// A table whose columns carry no `DEFAULT` at all, so anything a create
    /// stores for an absent field came from the write path.
    fn defaults_ddl() -> &'static str {
        "CREATE TABLE posts (
            id TEXT PRIMARY KEY,
            _revision INTEGER NOT NULL DEFAULT 0,
            title TEXT,
            rank REAL,
            kind TEXT,
            starts TEXT,
            tags TEXT,
            created_at TEXT,
            updated_at TEXT
        )"
    }

    /// The definition behind [`defaults_ddl`], every field with a default.
    fn defaults_def() -> CollectionDefinition {
        let mut def = test_def();
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .default_value(json!("Untitled"))
                .build(),
            FieldDefinition::builder("rank", FieldType::Number)
                .default_value(json!(7))
                .build(),
            FieldDefinition::builder("kind", FieldType::Select)
                .default_value(json!("draft"))
                .build(),
            FieldDefinition::builder("starts", FieldType::Date)
                .default_value(json!("2026-01-01"))
                .build(),
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .default_value(json!(["a", "b"]))
                .build(),
        ];
        def
    }

    /// Regression: a field the write omitted took its value from the column
    /// `DEFAULT`, a clause that can only be written when the column is created
    /// — so the stored default was whatever the table was born with. The create
    /// path applies the configured default itself, for every field type.
    #[test]
    fn an_omitted_field_stores_its_configured_default() {
        let (_dir, conn) = setup_db(defaults_ddl());
        let def = defaults_def();

        let doc = create(&conn, "posts", &def, &DocumentFields::new(), None).unwrap();

        assert_eq!(doc.get("title"), Some(&json!("Untitled")));
        assert_eq!(doc.get("rank"), Some(&json!(7)));
        assert_eq!(doc.get("kind"), Some(&json!("draft")));
        assert_eq!(doc.get("tags"), Some(&json!(["a", "b"])));
        assert!(
            doc.get("starts").is_some_and(|v| !v.is_null()),
            "a date default must be stored: {:?}",
            doc.get("starts")
        );
    }

    /// A default changed in the definition reaches the next document without a
    /// migration — the column `DEFAULT` still says the old value.
    #[test]
    fn a_changed_default_applies_without_a_migration() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT DEFAULT 'stale',
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = test_def();
        def.fields[0] = FieldDefinition::builder("title", FieldType::Text)
            .default_value(json!("current"))
            .build();

        let doc = create(&conn, "posts", &def, &DocumentFields::new(), None).unwrap();

        assert_eq!(doc.get("title"), Some(&json!("current")));
    }

    /// A sent value wins over the default — including an explicit `null`, which
    /// means "no value" and must not be overwritten by the default.
    #[test]
    fn a_sent_value_wins_over_the_default() {
        let (_dir, conn) = setup_db(defaults_ddl());
        let def = defaults_def();

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Sent"));
        data.insert("rank".to_string(), Value::Null);

        let doc = create(&conn, "posts", &def, &data, None).unwrap();

        assert_eq!(doc.get("title"), Some(&json!("Sent")));
        assert!(
            doc.get("rank").unwrap_or(&Value::Null).is_null(),
            "an explicit null must store NULL, not the default: {:?}",
            doc.get("rank")
        );
    }

    #[test]
    fn create_checkbox_defaults_to_zero() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                status TEXT,
                published INTEGER,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = test_def();
        def.fields
            .push(FieldDefinition::builder("published", FieldType::Checkbox).build());

        // Create without providing the checkbox field
        let data = DocumentFields::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();

        // An absent checkbox stores 0 and reads back as `false`
        let published = doc.get("published").unwrap();
        assert_eq!(published, &json!(false));
    }

    /// Regression: an absent checkbox with `default_value = true` must store
    /// `1`, honoring the configured default like every other field type's column
    /// `DEFAULT` — the write path previously forced `0` unconditionally.
    #[test]
    fn create_checkbox_honors_true_default() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                status TEXT,
                featured INTEGER,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = test_def();
        def.fields.push(
            FieldDefinition::builder("featured", FieldType::Checkbox)
                .default_value(json!(true))
                .build(),
        );

        // Create omitting the checkbox — an API create that doesn't send it.
        let data = DocumentFields::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();

        assert_eq!(
            doc.get("featured").unwrap(),
            &json!(true),
            "an absent checkbox with default_value=true must store 1"
        );
    }

    #[test]
    fn create_group_with_checkbox_sub_field_default() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                settings__featured INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("featured", FieldType::Checkbox).build(),
                ])
                .build(),
        ];
        let def = def;

        // Create without providing the checkbox group sub-field — should default to 0
        let data = DocumentFields::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let val = doc.get("settings__featured").unwrap();
        assert_eq!(val, &json!(false));
    }
}
