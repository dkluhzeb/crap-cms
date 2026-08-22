//! Scalar field validation. Called from `ValidationWalker::walk` for any field
//! that isn't a layout container (Group/Row/Collapsible/Tabs/Join).

use serde_json::Value;

use crate::{
    core::{BLOCK_TYPE_KEY, FieldDefinition, FieldType, validate::FieldError},
    db::{LocaleContext, query::helpers::prefixed_name, query::locale_write_column},
    hooks::lifecycle::validation::{
        checks,
        custom::{ValidateCtxSource, run_required_condition_inner},
        is_empty_value,
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

        // Localized required is enforced by the document-level completeness
        // check (against `required_locales`) whenever localization is active,
        // so skip the per-submit `required` check for locale-scoped fields here.
        // Non-locale-scoped fields (incl. join fields that aren't themselves
        // `localized`) keep the normal per-submit `required` check.
        let localization_active = self.ctx.locale_ctx.is_some_and(|c| c.config.is_enabled());
        let skip_required = self.ctx.is_draft
            || (field.is_locale_scoped(inherited_localized) && localization_active);

        // Validators see the operation (`create`/`update`) and the document id
        // being edited (derived from the validation context's `exclude_id`).
        let operation = if is_update { "update" } else { "create" };

        // A field is required either statically (`required = true`) or when its
        // `required_when` predicate returns truthy for this document. Skip the
        // predicate entirely when the required check itself is skipped (drafts,
        // locale-scoped fields): there is nothing to enforce, and a predicate
        // error must not be able to block a draft save.
        let required = if skip_required {
            false
        } else {
            field.required
                || match field.required_when.as_ref() {
                    None => false,
                    Some(required_when) => match run_required_condition_inner(
                        self.lua,
                        required_when.reference(),
                        &ValidateCtxSource {
                            data: self.document,
                            document: self.document,
                            collection: self.ctx.table,
                            field_name: &field.name,
                            locale: self.ctx.locale_ctx.map(LocaleContext::access_locale),
                            operation,
                            id: self.ctx.exclude_id,
                            options: required_when.options(),
                        },
                    ) {
                        Ok(req) => req,
                        Err(e) => {
                            errors.push(FieldError::new(
                                data_key.clone(),
                                format!(
                                    "required_when predicate '{}' failed: {e}",
                                    required_when.reference()
                                ),
                            ));
                            false
                        }
                    },
                }
        };

        checks::check_required(
            field,
            &data_key,
            value,
            required,
            skip_required,
            is_update,
            errors,
        );
        checks::check_row_bounds(
            field,
            &data_key,
            value,
            self.ctx.is_draft,
            is_update,
            errors,
        );
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
        checks::check_has_many_elements(
            field,
            &data_key,
            value,
            is_empty,
            self.ctx.is_draft,
            errors,
        );
        checks::check_date_field(field, &data_key, value, is_empty, errors);
        checks::check_custom_validate(
            self.lua,
            field,
            &data_key,
            value,
            &checks::CustomValidateCtx {
                data: self.document,
                table: self.ctx.table,
                locale: self.ctx.locale_ctx.map(LocaleContext::access_locale),
                operation,
                id: self.ctx.exclude_id,
            },
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
        // Resolve the exact column the write path stores into, via the shared
        // `locale_write_column` — so the uniqueness check can never query a
        // different column than the write targets (which would let a duplicate
        // slip through). Emitting an error on failure keeps this fail-closed.
        let Ok(col) =
            locale_write_column(data_key, field, self.ctx.locale_ctx, inherited_localized)
        else {
            errors.push(
                FieldError::with_key(
                    data_key.to_string(),
                    format!(
                        "{}: could not resolve locale column — cannot verify uniqueness",
                        field.name
                    ),
                    "validation.invalid_locale",
                )
                .with_param("field", field.name.clone()),
            );
            return None;
        };

        Some(col)
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
                .locale(self.ctx.locale_ctx.map(LocaleContext::access_locale))
                .operation(if self.ctx.exclude_id.is_some() {
                    "update"
                } else {
                    "create"
                })
                .id(self.ctx.exclude_id)
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

    /// Regression: the unique check resolves its column via the same
    /// `locale_write_column` the write path uses, so an out-of-config locale
    /// falls back to the **default-locale column** — the exact column the write
    /// stores into — rather than querying a different column (which would let a
    /// duplicate slip through). Here a locale not in `config.locales` falls back
    /// to `slug__en`, and a value colliding with the existing `slug__en` row is
    /// correctly caught as a unique violation.
    #[test]
    fn unique_check_uses_write_column_for_out_of_config_locale() {
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

        // A locale not present in `config.locales` — resolves (like the write
        // path) to the default-locale column `slug__en`.
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("xx".to_string()),
            config: locale_config,
        };

        let mut data = DocumentFields::new();
        // Collides with the existing `slug__en` value, so the check must fire.
        data.insert("slug".to_string(), json!("unique-en"));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .build(),
        );

        let errs = result
            .expect_err("duplicate on the resolved default-locale column must be caught")
            .errors;
        assert!(
            errs.iter()
                .any(|e| e.key.as_deref() == Some("validation.unique")),
            "expected a unique violation on slug__en, got: {errs:?}"
        );
    }

    fn en_de_config() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn required_title_localized() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .build(),
        ]
    }

    /// Completeness default (`required_locales` unset → default locale only):
    /// clearing a non-default locale is allowed as long as the default locale
    /// still has the value. Here `en` exists on the row, so nulling `de` passes.
    #[test]
    fn completeness_allows_clearing_nondefault_when_default_present() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT)");
        conn.setup("INSERT INTO test (id, title__en, title__de) VALUES ('p1', 'hello', 'hallo')");

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: en_de_config(),
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(null));

        let result = validate_fields_inner(
            &lua,
            &required_title_localized(),
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .exclude_id(Some("p1"))
                .build(),
        );
        assert!(
            result.is_ok(),
            "clearing a non-default locale must pass when the default locale is present: {:?}",
            result.err()
        );
    }

    /// `required_locales = "all"`: every locale must be filled. With `de` empty
    /// on the existing row, a (non-draft) write fails completeness even though
    /// the submitted `en` value is present.
    #[test]
    fn completeness_required_locales_all_enforces_each_locale() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT)");
        conn.setup("INSERT INTO test (id, title__en, title__de) VALUES ('p1', 'hello', NULL)");

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(crate::core::RequiredLocales::All)
                .build(),
        ];

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: en_de_config(),
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("hello"));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .exclude_id(Some("p1"))
                .build(),
        );
        let err = result.expect_err("missing 'de' translation must fail required_locales = all");
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.required_locale")),
            "expected a required_locale error, got: {:?}",
            err.errors
        );
    }

    /// Completeness covers join-backed localized fields (arrays/blocks/has-many):
    /// a required localized array with `required_locales = "all"` fails when a
    /// locale has no rows in the field's join table, even though the submitted
    /// (write) locale has items.
    #[test]
    fn completeness_join_field_required_locales_all() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY)");
        conn.setup(
            "CREATE TABLE test_items (id TEXT, parent_id TEXT, _locale TEXT, _order INTEGER, label TEXT)",
        );
        // The `en` locale has one row; `de` has none.
        conn.setup(
            "INSERT INTO test_items (id, parent_id, _locale, _order, label) VALUES ('i1','p1','en',0,'x')",
        );

        let label = FieldDefinition::builder("label", FieldType::Text).build();
        let items = FieldDefinition::builder("items", FieldType::Array)
            .required(true)
            .localized(true)
            .fields(vec![label])
            .required_locales(crate::core::RequiredLocales::All)
            .build();

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: en_de_config(),
        };

        let mut data = DocumentFields::new();
        data.insert("items".to_string(), json!([{ "label": "x" }]));

        let result = validate_fields_inner(
            &lua,
            &[items],
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .exclude_id(Some("p1"))
                .build(),
        );
        let err = result.expect_err("missing 'de' rows must fail required_locales = all");
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.required_locale")
                    && e.message.contains("'de'")),
            "expected a required_locale error for 'de', got: {:?}",
            err.errors
        );
    }

    /// Drafts are exempt from the completeness check — incomplete translations
    /// may be saved as drafts and only publishing enforces completeness.
    #[test]
    fn completeness_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT)");
        conn.setup("INSERT INTO test (id, title__en, title__de) VALUES ('p1', 'hello', NULL)");

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .localized(true)
                .required_locales(crate::core::RequiredLocales::All)
                .build(),
        ];

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: en_de_config(),
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("hello"));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .exclude_id(Some("p1"))
                .draft(true)
                .build(),
        );
        assert!(
            result.is_ok(),
            "drafts skip completeness: {:?}",
            result.err()
        );
    }

    /// `required` IS still enforced for a localized field in the DEFAULT locale
    /// — the default locale's content stays mandatory (and is the fallback
    /// source), so it cannot be emptied.
    #[test]
    fn required_localized_field_enforced_in_default_locale() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT)");

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: en_de_config(),
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(null));

        let result = validate_fields_inner(
            &lua,
            &required_title_localized(),
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .exclude_id(Some("p1"))
                .build(),
        );
        assert!(
            result.is_err(),
            "clearing a required localized field in the default locale must fail"
        );
    }

    /// A NON-localized required field is shared across locales and stays
    /// required regardless of the write locale.
    #[test]
    fn required_nonlocalized_field_enforced_in_nondefault_locale() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT)");

        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: en_de_config(),
        };

        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .required(true)
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("slug".to_string(), json!(null));

        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .locale_ctx(Some(&locale_ctx))
                .exclude_id(Some("p1"))
                .build(),
        );
        assert!(
            result.is_err(),
            "non-localized required field must stay required in any locale"
        );
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
