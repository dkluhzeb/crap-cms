use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, checkbox_value, validate::FieldError};

/// Reject a checkbox value that is not a boolean in any spelling the write
/// stores. Without this a `"maybe"`, an array or an object coerced silently to
/// `false` at the persist edge — the caller believed it had set the flag.
///
/// Accepted: a JSON boolean, a recognized spelling (`1`/`0`, `true`/`false`,
/// `yes`/`no`, `on`/`off` — trimmed, any case) and a number or numeric string
/// (non-zero is checked). An empty value is absence, not a bad boolean:
/// `required` owns that, and every other check skips it the same way.
pub(crate) fn check_checkbox_value(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if field.field_type != FieldType::Checkbox || is_empty {
        return;
    }

    let Some(value) = value else {
        return;
    };

    if checkbox_value(value).is_some() {
        return;
    }

    errors.push(
        FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be a boolean", field.name),
            "validation.invalid_boolean",
        )
        .with_param("field", field.name.clone()),
    );
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        core::{BlockDefinition, DocumentFields, FieldDefinition, FieldType, validate::FieldError},
        db::InMemoryConn,
        hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
    };

    /// Values no reader can turn into a checked/unchecked decision.
    fn rejected() -> Vec<Value> {
        vec![json!("maybe"), json!([]), json!({}), json!("2x")]
    }

    /// Every spelling the write stores as a flag, plus the absent forms the
    /// `required` check owns.
    fn accepted() -> Vec<Value> {
        vec![
            json!(true),
            json!(false),
            json!("on"),
            json!(1),
            json!("2"),
            json!(" 0 "),
            json!(null),
            json!(""),
        ]
    }

    /// Validate `data` against `fields` on a table created by `ddl`, returning
    /// the field errors (empty when the document is valid).
    fn errors_for(ddl: &str, fields: &[FieldDefinition], data: &DocumentFields) -> Vec<FieldError> {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup(ddl);

        validate_fields_inner(
            &lua,
            fields,
            data,
            &ValidationCtx::builder(&conn, "test").build(),
        )
        .err()
        .map(|e| e.errors)
        .unwrap_or_default()
    }

    fn one_checkbox(value: Value) -> DocumentFields {
        let mut data = DocumentFields::new();
        data.insert("active".to_string(), value);

        data
    }

    fn checkbox_fields() -> Vec<FieldDefinition> {
        vec![FieldDefinition::builder("active", FieldType::Checkbox).build()]
    }

    const CHECKBOX_DDL: &str = "CREATE TABLE test (id TEXT PRIMARY KEY, active INTEGER)";

    #[test]
    fn a_top_level_checkbox_rejects_a_non_boolean() {
        for bad in rejected() {
            let errors = errors_for(CHECKBOX_DDL, &checkbox_fields(), &one_checkbox(bad.clone()));
            assert!(
                errors
                    .iter()
                    .any(|e| e.key.as_deref() == Some("validation.invalid_boolean")
                        && e.message == "active must be a boolean"),
                "{bad:?} must be rejected, got: {errors:?}"
            );
        }
    }

    #[test]
    fn a_top_level_checkbox_accepts_every_recognized_spelling() {
        for ok in accepted() {
            let errors = errors_for(CHECKBOX_DDL, &checkbox_fields(), &one_checkbox(ok.clone()));
            assert!(errors.is_empty(), "{ok:?} must pass, got: {errors:?}");
        }
    }

    /// The admin form's own encodings: an unchecked box is normalized to `"0"`,
    /// a checked one submits `"on"`, a locale-locked one a hidden `"1"`/`"0"`.
    #[test]
    fn the_admin_form_encodings_pass() {
        for ok in [json!("0"), json!("1"), json!("on")] {
            let errors = errors_for(CHECKBOX_DDL, &checkbox_fields(), &one_checkbox(ok.clone()));
            assert!(errors.is_empty(), "{ok:?} must pass, got: {errors:?}");
        }
    }

    #[test]
    fn a_checkbox_inside_a_group_is_checked() {
        let fields = vec![
            FieldDefinition::builder("flags", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("active", FieldType::Checkbox).build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("flags__active".to_string(), json!("maybe"));

        let errors = errors_for(
            "CREATE TABLE test (id TEXT PRIMARY KEY, flags__active INTEGER)",
            &fields,
            &data,
        );
        assert_eq!(
            errors.first().map(|e| e.field.as_str()),
            Some("flags__active"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_checkbox_inside_an_array_row_is_checked() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("active", FieldType::Checkbox).build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{ "active": "maybe" }]));

        let errors = errors_for("CREATE TABLE test (id TEXT PRIMARY KEY)", &fields, &data);
        assert!(
            errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_boolean")),
            "{errors:?}"
        );

        // A recognized spelling in the same position still passes.
        let mut good = DocumentFields::new();
        good.insert("items".to_string(), json!([{ "active": " 0 " }]));
        assert!(
            errors_for("CREATE TABLE test (id TEXT PRIMARY KEY)", &fields, &good).is_empty(),
            "a padded '0' inside an array row must pass"
        );
    }

    #[test]
    fn a_checkbox_inside_a_group_inside_an_array_row_is_checked() {
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("flags", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("active", FieldType::Checkbox).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{ "flags": { "active": [] } }]));

        let errors = errors_for("CREATE TABLE test (id TEXT PRIMARY KEY)", &fields, &data);
        assert!(
            errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_boolean")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_checkbox_inside_a_blocks_row_is_checked() {
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "toggle",
                    vec![FieldDefinition::builder("active", FieldType::Checkbox).build()],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert(
            "content".to_string(),
            json!([{ "_block_type": "toggle", "active": {} }]),
        );

        let errors = errors_for("CREATE TABLE test (id TEXT PRIMARY KEY)", &fields, &data);
        assert!(
            errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_boolean")),
            "{errors:?}"
        );
    }
}
