//! Scalar field validation. Called from `ValidationWalker::walk` for any field
//! that isn't a layout container (Group/Row/Collapsible/Tabs/Join).

use serde_json::Value;

use crate::{
    core::{FieldDefinition, FieldType, validate::FieldError},
    db::{LocaleMode, query::helpers::prefixed_name, query::sanitize_locale},
    hooks::lifecycle::validation::{
        checks, is_empty_value,
        richtext_attrs::{RichtextValidationCtx, validate_richtext_node_attrs},
        sub_fields::{SubFieldParams, validate_sub_fields_inner},
    },
};

use super::dispatch::ValidationWalker;

impl ValidationWalker<'_> {
    /// Validate a single scalar field (not Group/Row/Collapsible/Tabs).
    /// Dispatches to individual check functions in `checks` module.
    pub(super) fn scalar(
        &self,
        field: &FieldDefinition,
        prefix: &str,
        inherited_localized: bool,
        errors: &mut Vec<FieldError>,
    ) {
        let data_key = prefixed_name(prefix, &field.name);

        let value = self.data.get(&data_key);
        let is_empty = is_empty_value(value);
        let is_update = self.ctx.exclude_id.is_some();

        checks::check_required(
            field,
            &data_key,
            value,
            is_empty,
            self.ctx.is_draft,
            is_update,
            errors,
        );
        checks::check_row_bounds(field, &data_key, value, self.ctx.is_draft, errors);
        checks::check_polymorphic_allowlist(field, &data_key, value, errors);

        self.validate_array_or_blocks_rows(field, &data_key, value, errors);

        if let Some(col_name) =
            self.resolve_unique_check_column(field, &data_key, inherited_localized, errors)
        {
            checks::check_unique(
                field, &data_key, &col_name, value, is_empty, self.ctx, errors,
            );
        }
        checks::check_length_bounds(field, &data_key, value, is_empty, errors);
        checks::check_numeric_bounds(field, &data_key, value, is_empty, errors);
        checks::check_email_format(field, &data_key, value, is_empty, errors);
        checks::check_option_valid(field, &data_key, value, is_empty, errors);
        checks::check_has_many_elements(field, &data_key, value, is_empty, errors);
        checks::check_date_field(field, &data_key, value, is_empty, errors);
        checks::check_custom_validate(
            self.lua,
            field,
            &data_key,
            value,
            self.data,
            self.ctx.table,
            errors,
        );

        self.validate_richtext_node_attrs_field(field, &data_key, value, is_empty, errors);
    }

    /// For `Array`/`Blocks` fields, walk each row and recurse into sub-fields.
    /// Draft mode still validates sub-fields (format, bounds, etc.) — only
    /// `required` checks are skipped inside sub-field validation via the
    /// `is_draft` flag.
    fn validate_array_or_blocks_rows(
        &self,
        field: &FieldDefinition,
        data_key: &str,
        value: Option<&Value>,
        errors: &mut Vec<FieldError>,
    ) {
        let has_sub_structure = !field.fields.is_empty() || !field.blocks.is_empty();
        if !matches!(field.field_type, FieldType::Array | FieldType::Blocks) || !has_sub_structure {
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
            };
            validate_sub_fields_inner(&params, sub_fields, row_obj, errors);
        }
    }

    /// Compute the actual DB column name for the unique check. Localized
    /// fields store data in suffixed columns (e.g. `slug__en`).
    ///
    /// Returns `None` (and emits a validation error) when locale sanitization
    /// fails — silently skipping the unique check could allow duplicates to
    /// slip through.
    fn resolve_unique_check_column(
        &self,
        field: &FieldDefinition,
        data_key: &str,
        inherited_localized: bool,
        errors: &mut Vec<FieldError>,
    ) -> Option<String> {
        let is_localized =
            (inherited_localized || field.localized) && self.ctx.locale_ctx.is_some();
        let Some(lctx) = self.ctx.locale_ctx.filter(|_| is_localized) else {
            return Some(data_key.to_string());
        };

        let locale = match &lctx.mode {
            LocaleMode::Single(l) => l.as_str(),
            _ => lctx.config.default_locale.as_str(),
        };

        if let Ok(l) = sanitize_locale(locale) {
            return Some(format!("{data_key}__{l}"));
        }

        errors.push(
            FieldError::with_key(
                data_key.to_string(),
                format!(
                    "{}: invalid locale '{}' — cannot verify uniqueness",
                    field.name, locale,
                ),
                "validation.invalid_locale",
            )
            .with_param("field", field.name.clone())
            .with_param("locale", locale.to_string()),
        );
        None
    }

    /// Validate custom-node attrs within a `Richtext` field's content.
    /// No-op for non-richtext fields, empty values, or fields without
    /// custom nodes registered.
    fn validate_richtext_node_attrs_field(
        &self,
        field: &FieldDefinition,
        data_key: &str,
        value: Option<&Value>,
        is_empty: bool,
        errors: &mut Vec<FieldError>,
    ) {
        if field.field_type != FieldType::Richtext || is_empty || field.admin.nodes.is_empty() {
            return;
        }
        let Some(registry) = self.ctx.registry else {
            return;
        };
        let Some(Value::String(content)) = value else {
            return;
        };
        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(self.lua, registry, self.ctx.table)
                .draft(self.ctx.is_draft)
                .build(),
            content,
            data_key,
            field,
            errors,
        );
    }
}

/// Resolve which sub-field schema applies to one Array/Blocks row.
/// For `Blocks`, looks up the block definition by `_block_type`; emits
/// an error and returns `None` when the type is unknown. For `Array`,
/// always returns the field's own sub-fields.
fn resolve_row_sub_fields<'def>(
    field: &'def FieldDefinition,
    row_obj: &serde_json::Map<String, Value>,
    data_key: &str,
    idx: usize,
    errors: &mut Vec<FieldError>,
) -> Option<&'def [FieldDefinition]> {
    if field.field_type != FieldType::Blocks {
        return Some(&field.fields);
    }
    let block_type = row_obj
        .get("_block_type")
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
    use crate::config::LocaleConfig;
    use crate::core::DocumentFields;
    use crate::core::Registry;
    use crate::core::RichtextNodeDef;
    use crate::core::{FieldAdmin, FieldDefinition, FieldType};
    use crate::db::{InMemoryConn, LocaleContext, LocaleMode};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;

    #[test]
    fn test_validate_date_inside_collapsible_top_level() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, pub_date TEXT)");
        let fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("pub_date", FieldType::Date).build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("pub_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Invalid date inside collapsible at top-level should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_date_inside_row_top_level() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, event_date TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("event_date", FieldType::Date).build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("event_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Invalid date inside row at top-level should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    // --- Richtext node attr validation integration tests ---

    #[test]
    fn test_richtext_node_attr_required_through_validation_pipeline() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, content TEXT)");

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .build(),
                    FieldDefinition::builder("url", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        let fields = vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        let json_content =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"","url":""}}]}"#;
        let mut data = DocumentFields::new();
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages")
                .registry(&reg)
                .build(),
        );

        assert!(result.is_err(), "empty required node attrs should fail");
        let errs = result.unwrap_err().errors;
        assert_eq!(errs.len(), 2);
        assert_eq!(errs[0].field, "content[cta#0].text");
        assert_eq!(errs[1].field, "content[cta#0].url");
    }

    #[test]
    fn test_richtext_node_attr_valid_passes_pipeline() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, content TEXT)");

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        let fields = vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        let json_content =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"Click me"}}]}"#;
        let mut data = DocumentFields::new();
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages")
                .registry(&reg)
                .build(),
        );

        assert!(result.is_ok(), "valid node attrs should pass");
    }

    #[test]
    fn test_richtext_node_attr_no_registry_skips_validation() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, content TEXT)");

        let fields = vec![
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        // Content with invalid data, but no registry provided
        let json_content = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":""}}]}"#;
        let mut data = DocumentFields::new();
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages").build(), // no registry
        );

        assert!(
            result.is_ok(),
            "without registry, node attr validation is skipped"
        );
    }

    #[test]
    fn test_richtext_node_attrs_alongside_regular_field_errors() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE pages (id TEXT PRIMARY KEY, title TEXT, content TEXT)");

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("content", FieldType::Richtext)
                .admin(
                    FieldAdmin::builder()
                        .nodes(vec!["cta".to_string()])
                        .richtext_format("json")
                        .build(),
                )
                .build(),
        ];

        let json_content = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":""}}]}"#;
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(""));
        data.insert("content".to_string(), json!(json_content));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "pages")
                .registry(&reg)
                .build(),
        );

        assert!(result.is_err());
        let errs = result.unwrap_err().errors;
        assert_eq!(errs.len(), 2);
        // Regular field error first, then node attr error
        assert_eq!(errs[0].field, "title");
        assert_eq!(errs[1].field, "content[cta#0].text");
    }

    /// Regression: when locale sanitization fails, validation must emit an
    /// error rather than silently skipping the unique check (which could allow
    /// duplicates to slip through in the localized column).
    #[test]
    fn test_invalid_locale_emits_validation_error() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT, slug__en TEXT);
             INSERT INTO test (id, slug, slug__en) VALUES ('existing', 'taken', 'unique-en');",
        );

        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .unique(true)
                .localized(true)
                .build(),
        ];

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string()],
            fallback: false,
        };

        // Use a locale string that sanitizes to empty (only special chars)
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("@!#$%".to_string()),
            config: locale_config,
        };

        let mut data = DocumentFields::new();
        data.insert("slug".to_string(), json!("taken"));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .build(),
        );

        assert!(result.is_err(), "Invalid locale must fail validation");
        let errs = result.unwrap_err().errors;
        assert_eq!(errs.len(), 1);
        assert!(
            errs[0].message.contains("invalid locale"),
            "Error should mention invalid locale, got: {}",
            errs[0].message,
        );
        assert_eq!(errs[0].key.as_deref(), Some("validation.invalid_locale"));
    }
}
