//! Scalar field validation. Called from `ValidationWalker::walk` for any field
//! that isn't a layout container (Group/Row/Collapsible/Tabs/Join). Array and
//! blocks rows recurse through `rows`; rich text custom-node attrs through
//! `richtext`.

use serde_json::Value;

use crate::{
    core::{FieldDefinition, validate::FieldError},
    db::{
        LocaleContext,
        query::{
            helpers::{column_value, prefixed_name, tz_column},
            locale_write_column,
        },
    },
    hooks::lifecycle::validation::{
        checks,
        custom::{ValidateCtxSource, run_required_condition_inner},
        is_empty_value,
    },
};

use super::dispatch::ValidationWalker;

impl ValidationWalker<'_> {
    /// Date format and bounds, plus — for a timezone-enabled date — that the
    /// local time exists in its zone.
    fn check_date(
        &self,
        field: &FieldDefinition,
        data_key: &str,
        value: Option<&Value>,
        is_empty: bool,
        errors: &mut Vec<FieldError>,
    ) {
        checks::check_date_field(field, data_key, value, is_empty, errors);
        checks::check_local_time_exists(
            field,
            data_key,
            value,
            self.data.get(&tz_column(data_key)).and_then(Value::as_str),
            errors,
        );
    }

    /// Whether `field` is required for this write: statically (`required`) or
    /// when its `required_when` predicate holds for the document. A predicate
    /// failure is reported as a field error and reads as not required.
    fn field_required(
        &self,
        field: &FieldDefinition,
        data_key: &str,
        operation: &str,
        errors: &mut Vec<FieldError>,
    ) -> bool {
        let Some(required_when) = field.required_when.as_ref() else {
            return field.required;
        };

        if field.required {
            return true;
        }

        let source = ValidateCtxSource {
            data: self.document,
            document: self.document,
            collection: self.ctx.table,
            field_name: &field.name,
            locale: self.ctx.locale_ctx.map(LocaleContext::access_locale),
            operation,
            id: self.ctx.exclude_id,
            options: required_when.options(),
        };

        match run_required_condition_inner(self.lua, required_when.reference(), &source) {
            Ok(required) => required,
            Err(e) => {
                errors.push(FieldError::new(
                    data_key.to_owned(),
                    format!(
                        "required_when predicate '{}' failed: {e}",
                        required_when.reference()
                    ),
                ));
                false
            }
        }
    }

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

        let required = if skip_required {
            false
        } else {
            self.field_required(field, &data_key, operation, errors)
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
        checks::check_relationship_shape(field, &data_key, value, errors);
        checks::check_polymorphic_allowlist(field, &data_key, value, errors);

        self.validate_array_or_blocks_rows(field, &data_key, value, errors);

        if let Some(col_name) =
            self.resolve_unique_check_column(field, &data_key, inherited_localized, errors)
        {
            let zone = self.data.get(&tz_column(&data_key)).and_then(Value::as_str);
            let stored = value.map(|v| column_value(field, v, zone));
            checks::check_unique(
                field, &data_key, &col_name, stored, is_empty, self.ctx, errors,
            );
        }
        checks::check_richtext_value(
            &checks::RichtextCheck::new(field, &data_key, value)
                .registry(self.ctx.registry)
                .stored(self.stored),
            errors,
        );
        checks::check_length_bounds(field, &data_key, value, is_empty, errors);
        checks::check_numeric_bounds(field, &data_key, value, is_empty, errors);
        checks::check_checkbox_value(field, &data_key, value, is_empty, errors);
        checks::check_email_format(field, &data_key, value, is_empty, errors);
        checks::check_option_valid(
            &checks::OptionCheck::new(field, &data_key, value, is_empty).stored(self.stored),
            errors,
        );
        checks::check_has_many_elements(
            &checks::HasManyCheck::new(field, &data_key, value, is_empty)
                .draft(self.ctx.is_draft)
                .update(is_update),
            errors,
        );
        self.check_date(field, &data_key, value, is_empty, errors);
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
}
