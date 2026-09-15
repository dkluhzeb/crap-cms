//! INSERT params for a single leaf (scalar) field and its companions.

use anyhow::Result;
use serde_json::Value;

use crate::{
    core::{DocumentFields, FieldDefinition, FieldType},
    db::{
        DbConnection, DbValue, LocaleContext,
        query::{
            helpers::{
                column_value, companion_writes, prefixed_name, tz_column,
                validate_no_null_byte_json,
            },
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
        if field.field_type == FieldType::Checkbox {
            // Absent checkbox on create honors a configured boolean default. The
            // admin form normalizes an unchecked box to an explicit `0`, so a
            // genuine absence here is an API create (Lua / gRPC / MCP) that
            // omitted the field — which should inherit the default like every
            // other field type does via its column `DEFAULT`. Falls back to 0.
            let default_on = matches!(field.default_value.as_ref(), Some(Value::Bool(true)));
            collector.push(conn, &col_name, DbValue::Integer(i64::from(default_on)));
        }

        return Ok(());
    };

    validate_no_null_byte_json(&field.field_type, &data_key, value)?;

    let zone = data.get(&tz_column(&data_key)).and_then(Value::as_str);
    collector.push(conn, &col_name, column_value(field, value, zone));

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldType},
        db::query::write::create::{
            create,
            test_support::{setup_db, test_def},
        },
    };

    #[test]
    fn create_checkbox_defaults_to_zero() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
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

        // Checkbox should default to 0 (integer)
        let published = doc.get("published").unwrap();
        assert_eq!(published, &json!(0));
    }

    /// Regression: an absent checkbox with `default_value = true` must store
    /// `1`, honoring the configured default like every other field type's column
    /// `DEFAULT` — the write path previously forced `0` unconditionally.
    #[test]
    fn create_checkbox_honors_true_default() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
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
            &json!(1),
            "an absent checkbox with default_value=true must store 1"
        );
    }

    #[test]
    fn create_group_with_checkbox_sub_field_default() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
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
        assert_eq!(val, &json!(0));
    }
}
