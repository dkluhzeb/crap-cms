//! Array and blocks rows: each row is validated against its sub-field schema
//! (the block's, for blocks), recursing into the sub-fields.

use serde_json::{Map, Value};

use crate::{
    core::{BLOCK_TYPE_KEY, FieldDefinition, FieldType, validate::FieldError},
    db::LocaleContext,
    hooks::lifecycle::validation::sub_fields::{SubFieldParams, validate_sub_fields_inner},
};

use super::dispatch::ValidationWalker;

impl ValidationWalker<'_> {
    /// For `Array`/`Blocks` fields, walk each row and recurse into sub-fields.
    /// Draft mode still validates sub-fields (format, bounds, etc.) — only
    /// `required` checks are skipped inside sub-field validation via the
    /// `is_draft` flag.
    pub(super) fn validate_array_or_blocks_rows(
        &self,
        field: &FieldDefinition,
        data_key: &str,
        value: Option<&Value>,
        errors: &mut Vec<FieldError>,
    ) {
        let has_sub_structure = !field.fields.is_empty() || !field.blocks.is_empty();
        if !field.field_type.has_rows() || !has_sub_structure {
            return;
        }
        let Some(Value::Array(rows)) = value else {
            return;
        };

        for (idx, row) in rows.iter().enumerate() {
            let Some(row_obj) = row.as_object() else {
                errors.push(
                    FieldError::with_key(
                        format!("{data_key}[{idx}]"),
                        format!("{} row {} must be an object", field.name, idx),
                        "validation.invalid_row_type",
                    )
                    .with_param("field", field.name.clone())
                    .with_param("index", idx.to_string()),
                );
                continue;
            };
            let Some(sub_fields) = resolve_row_sub_fields(field, row_obj, data_key, idx, errors)
            else {
                continue;
            };
            let params = SubFieldParams {
                lua: self.lua,
                parent_name: data_key,
                idx,
                table: self.ctx.table,
                registry: self.ctx.registry,
                is_draft: self.ctx.is_draft,
                locale: self.ctx.locale_ctx.map(LocaleContext::access_locale),
                operation: if self.ctx.exclude_id.is_some() {
                    "update"
                } else {
                    "create"
                },
                id: self.ctx.exclude_id,
                document: self.document,
                stored: self.stored,
            };
            validate_sub_fields_inner(&params, sub_fields, row_obj, errors);
        }
    }
}

/// Resolve which sub-field schema applies to one Array/Blocks row.
/// For `Blocks`, looks up the block definition by `_block_type`; emits
/// an error and returns `None` when the type is unknown. For `Array`,
/// always returns the field's own sub-fields.
fn resolve_row_sub_fields<'def>(
    field: &'def FieldDefinition,
    row_obj: &Map<String, Value>,
    data_key: &str,
    idx: usize,
    errors: &mut Vec<FieldError>,
) -> Option<&'def [FieldDefinition]> {
    if field.field_type != FieldType::Blocks {
        return Some(&field.fields);
    }
    let block_type = row_obj
        .get(BLOCK_TYPE_KEY)
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(bd) = field.blocks.iter().find(|b| b.block_type == block_type) {
        Some(&bd.fields)
    } else {
        errors.push(
            FieldError::with_key(
                format!("{data_key}[{idx}]"),
                format!(
                    "{} row {} has unknown block type '{}'",
                    field.name, idx, block_type
                ),
                "validation.unknown_block_type",
            )
            .with_param("field", field.name.clone())
            .with_param("index", idx.to_string())
            .with_param("block_type", block_type.to_string()),
        );
        None
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use crate::{
        config::LocaleConfig,
        core::{DocumentFields, FieldDefinition, FieldType},
        db::{InMemoryConn, LocaleContext, LocaleMode},
        hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
    };

    fn en_de_config() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    /// A custom `validate` on a sub-field inside an array receives the content
    /// `ctx.locale` (regression for threading locale into sub-field validation).
    /// The validator echoes `ctx.locale` back as its error string.
    #[test]
    fn sub_field_validator_receives_content_locale() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                echo_locale = function(value, ctx) return ctx.locale end
            }
        "#,
        )
        .exec()
        .unwrap();

        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY)");

        let label = FieldDefinition::builder("label", FieldType::Text)
            .validate("validators.echo_locale")
            .build();
        let items = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![label])
            .build();

        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{ "label": "x" }]));

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: en_de_config(),
        };
        let result = validate_fields_inner(
            &lua,
            &[items],
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .build(),
        );
        let err = result.expect_err("validator echoes locale as an error");
        assert!(
            err.errors.iter().any(|e| e.message == "de"),
            "sub-field validator should see ctx.locale = 'de', got: {:?}",
            err.errors
        );
    }
}
