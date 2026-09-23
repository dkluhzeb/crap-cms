//! Richtext attribute validation: per-node-instance checks (required, length,
//! numeric, email, select, date, custom Lua) plus the public entry point.

use std::collections::HashMap;

use mlua::Lua;
use serde_json::Value;

use crate::{
    core::{FieldDefinition, registry::Registry, validate::FieldError},
    hooks::lifecycle::validation::{
        checks,
        checks::OptionCheck,
        custom::{ValidateCtxSource, run_validate_function_inner},
        is_empty_value,
    },
};

use super::extract::{
    NodeInstance, extract_nodes_from_html, extract_nodes_from_json, json_document,
};

/// Bundled context for richtext node attr validation.
pub(crate) struct RichtextValidationCtx<'a> {
    pub lua: &'a Lua,
    pub registry: &'a Registry,
    pub collection: &'a str,
    pub is_draft: bool,
    /// Content locale this write targets — exposed to custom node-attr
    /// validators as `ctx.locale`. `None` when localization is disabled.
    pub locale: Option<&'a str>,
    /// `"create"` or `"update"`.
    pub operation: &'a str,
    /// The document id on `update`; `None` on `create`.
    pub id: Option<&'a str>,
}

impl<'a> RichtextValidationCtx<'a> {
    /// Create a builder with the required fields.
    pub fn builder(
        lua: &'a Lua,
        registry: &'a Registry,
        collection: &'a str,
    ) -> RichtextValidationCtxBuilder<'a> {
        RichtextValidationCtxBuilder {
            lua,
            registry,
            collection,
            is_draft: false,
            locale: None,
            operation: "create",
            id: None,
        }
    }
}

/// Builder for [`RichtextValidationCtx`].
pub(crate) struct RichtextValidationCtxBuilder<'a> {
    lua: &'a Lua,
    registry: &'a Registry,
    collection: &'a str,
    is_draft: bool,
    locale: Option<&'a str>,
    operation: &'a str,
    id: Option<&'a str>,
}

impl<'a> RichtextValidationCtxBuilder<'a> {
    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    pub fn locale(mut self, locale: Option<&'a str>) -> Self {
        self.locale = locale;
        self
    }

    pub fn operation(mut self, operation: &'a str) -> Self {
        self.operation = operation;
        self
    }

    pub fn id(mut self, id: Option<&'a str>) -> Self {
        self.id = id;
        self
    }

    pub fn build(self) -> RichtextValidationCtx<'a> {
        RichtextValidationCtx {
            lua: self.lua,
            registry: self.registry,
            collection: self.collection,
            is_draft: self.is_draft,
            locale: self.locale,
            operation: self.operation,
            id: self.id,
        }
    }
}

/// Validate all custom node attrs within a richtext field's content.
///
/// Extracts custom nodes from the content (JSON or HTML format), then runs
/// the same validation checks used for regular fields on each node attr. A
/// JSON-format value may be the document's text or the document object; one
/// that is neither (unparseable, too deep, another type) is refused, since its
/// nodes cannot be checked.
///
/// Error field names use the format `"{field_name}[{node_type}#{index}].{attr_name}"`
/// to make errors identifiable (e.g., `"content[cta#0].url"`).
pub(crate) fn validate_richtext_node_attrs(
    ctx: &RichtextValidationCtx<'_>,
    content: &Value,
    field_name: &str,
    field: &FieldDefinition,
    errors: &mut Vec<FieldError>,
) {
    let known_nodes = known_nodes_with_attrs(ctx.registry, field);

    if known_nodes.is_empty() {
        return;
    }

    let instances = if field.parses_json() {
        let Some(doc) = json_document(content) else {
            errors.push(invalid_document_error(field_name, field));
            return;
        };
        extract_nodes_from_json(&doc, &known_nodes)
    } else {
        let Value::String(html) = content else {
            return;
        };
        extract_nodes_from_html(html, &known_nodes)
    };

    for inst in &instances {
        let attr_defs = match known_nodes.get(inst.node_type.as_str()) {
            Some(defs) => *defs,
            None => continue,
        };

        validate_node_instance(ctx, inst, attr_defs, field_name, errors);
    }
}

/// The field's declared nodes that have attrs, by node name.
fn known_nodes_with_attrs<'r>(
    registry: &'r Registry,
    field: &FieldDefinition,
) -> HashMap<&'r str, &'r [FieldDefinition]> {
    field
        .admin
        .nodes
        .iter()
        .filter_map(|name| registry.get_richtext_node(name))
        .filter(|node_def| !node_def.attrs.is_empty())
        .map(|node_def| (node_def.name.as_str(), node_def.attrs.as_slice()))
        .collect()
}

fn invalid_document_error(field_name: &str, field: &FieldDefinition) -> FieldError {
    FieldError::with_key(
        field_name,
        format!("{} must be a valid rich text JSON document", field.name),
        "validation.invalid_richtext_json",
    )
    .with_param("field", field.name.clone())
}

/// Validate a single node instance's attrs against their field definitions.
fn validate_node_instance(
    ctx: &RichtextValidationCtx<'_>,
    inst: &NodeInstance,
    attr_defs: &[FieldDefinition],
    field_name: &str,
    errors: &mut Vec<FieldError>,
) {
    for attr_def in attr_defs {
        let data_key = format!(
            "{}[{}#{}].{}",
            field_name, inst.node_type, inst.index, attr_def.name
        );
        let value = inst.attrs.get(&attr_def.name);
        let is_empty = is_empty_value(value);

        // Required check (skip for drafts) — presence judged by the same
        // predicate as top-level fields and array/blocks sub-fields.
        if attr_def.required
            && !ctx.is_draft
            && !checks::is_value_present(attr_def, value, is_empty)
        {
            errors.push(
                FieldError::with_key(
                    &data_key,
                    format!("{} is required", attr_def.name),
                    "validation.required",
                )
                .with_param("field", attr_def.name.clone()),
            );
            continue;
        }

        // A `has_many` node attr (Text/Number/Select/Radio list) gets the same
        // per-element + count validation as at the top level and in array/blocks
        // rows — previously this leaf site skipped it. (Row-bounds and the
        // polymorphic allowlist don't apply: a node attr can't be an
        // array/blocks or a relationship field.)
        checks::check_has_many_elements(
            &checks::HasManyCheck::new(attr_def, &data_key, value, is_empty).draft(ctx.is_draft),
            errors,
        );

        if is_empty {
            continue;
        }

        checks::check_length_bounds(attr_def, &data_key, value, is_empty, errors);
        checks::check_numeric_bounds(attr_def, &data_key, value, is_empty, errors);
        checks::check_email_format(attr_def, &data_key, value, is_empty, errors);
        checks::check_option_valid(
            &OptionCheck::new(attr_def, &data_key, value, is_empty),
            errors,
        );
        checks::check_date_field(attr_def, &data_key, value, is_empty, errors);

        // Custom Lua validate function
        if let Some(ref validate) = attr_def.validate
            && let Some(val) = value
        {
            let validate_ref = validate.reference();
            let data: HashMap<String, Value> = inst.attrs.clone();
            match run_validate_function_inner(
                ctx.lua,
                validate_ref,
                val,
                &ValidateCtxSource {
                    data: &data,
                    document: &data,
                    collection: ctx.collection,
                    field_name: &attr_def.name,
                    locale: ctx.locale,
                    operation: ctx.operation,
                    id: ctx.id,
                    options: validate.options(),
                },
            ) {
                Ok(Some(err_msg)) => {
                    errors.push(FieldError::new(data_key.clone(), err_msg));
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        "Custom validate function '{}' for node attr '{}' failed: {}",
                        validate_ref,
                        attr_def.name,
                        e,
                    );

                    // Fail closed: an erroring validator must reject the
                    // write, exactly like top-level fields (checks/custom.rs)
                    // and array/blocks sub-fields do.
                    errors.push(
                        FieldError::with_key(
                            data_key.clone(),
                            format!("Validation failed (internal error in '{validate_ref}')"),
                            "validation.custom_error",
                        )
                        .with_param("field", attr_def.name.clone()),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{
        FieldAdmin, FieldDefinition, FieldType, LocalizedString, Registry, SelectOption,
        richtext::RichtextNodeDef,
    };

    fn make_registry_with_cta() -> Registry {
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("text", FieldType::Text)
                        .required(true)
                        .min_length(2)
                        .max_length(100)
                        .build(),
                    FieldDefinition::builder("url", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );
        reg
    }

    fn make_richtext_field(nodes: Vec<String>, format: &str) -> FieldDefinition {
        FieldDefinition::builder("content", FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .richtext_format(format)
                    .nodes(nodes)
                    .build(),
            )
            .build()
    }

    /// Regression: unparseable JSON-format content found no nodes, so its
    /// node attrs went unchecked; it is now refused.
    #[test]
    fn validate_richtext_unparseable_json_is_refused() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from("not json"),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].key.as_deref(),
            Some("validation.invalid_richtext_json")
        );
    }

    /// Regression: a JSON-format document sent as an object (MCP, Lua tables)
    /// skipped node-attr validation, which read only strings.
    #[test]
    fn validate_richtext_document_object_is_checked() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        let doc = json!({
            "type": "doc",
            "content": [{ "type": "cta", "attrs": { "text": "", "url": "" } }]
        });
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &doc,
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 2, "both required attrs are empty");
    }

    /// Regression: extraction searched attribute substrings, so a decoy
    /// `data-type` inside another attribute and entity-encoded attrs (the
    /// editor's own serialization) escaped the check.
    #[test]
    fn validate_richtext_html_reads_attrs_like_the_browser() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "html");
        let html = concat!(
            r#"<CRAP-NODE data-x='data-type="other"' data-type="cta" "#,
            r#"data-attrs="{&quot;text&quot;:&quot;&quot;,&quot;url&quot;:&quot;&quot;}">"#,
            "</crap-node>",
        );
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(html),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 2, "both required attrs are empty: {errors:?}");
    }

    // --- Validation tests ---

    #[test]
    fn validate_richtext_required_attr_missing() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"","url":""}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 2, "both text and url are required");
        assert!(errors[0].field.contains("content[cta#0].text"));
        assert!(errors[1].field.contains("content[cta#0].url"));
    }

    #[test]
    fn validate_richtext_length_bounds() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"X","url":"/ok"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1, "text too short (min_length=2)");
        assert!(errors[0].field.contains("content[cta#0].text"));
    }

    #[test]
    fn validate_richtext_valid_passes() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        let json =
            r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"Click me","url":"/go"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert!(errors.is_empty(), "valid data should produce no errors");
    }

    #[test]
    fn validate_richtext_html_format() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "html");
        let html =
            r#"<p>Hi</p><crap-node data-type="cta" data-attrs='{"text":"","url":""}'></crap-node>"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(html),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 2, "both text and url required in HTML format");
    }

    #[test]
    fn validate_richtext_no_nodes_configured() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec![], "json");
        let json = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"","url":""}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert!(errors.is_empty(), "no nodes configured = no validation");
    }

    #[test]
    fn validate_richtext_email_attr() {
        let lua = Lua::new();
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("contact", "Contact")
                .attrs(vec![
                    FieldDefinition::builder("email", FieldType::Email)
                        .required(true)
                        .build(),
                ])
                .build(),
        );
        let field = make_richtext_field(vec!["contact".to_string()], "json");
        let json =
            r#"{"type":"doc","content":[{"type":"contact","attrs":{"email":"not-an-email"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("email"));
    }

    #[test]
    fn validate_richtext_select_option() {
        let lua = Lua::new();
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("alert", "Alert")
                .attrs(vec![
                    FieldDefinition::builder("style", FieldType::Select)
                        .options(vec![
                            SelectOption::new(LocalizedString::Plain("Info".to_string()), "info"),
                            SelectOption::new(
                                LocalizedString::Plain("Warning".to_string()),
                                "warning",
                            ),
                        ])
                        .build(),
                ])
                .build(),
        );
        let field = make_richtext_field(vec!["alert".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"alert","attrs":{"style":"invalid"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("valid option"));
    }

    #[test]
    fn validate_richtext_numeric_bounds() {
        let lua = Lua::new();
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("counter", "Counter")
                .attrs(vec![
                    FieldDefinition::builder("count", FieldType::Number)
                        .min(1.0)
                        .max(10.0)
                        .build(),
                ])
                .build(),
        );
        let field = make_richtext_field(vec!["counter".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"counter","attrs":{"count":"0"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains('1'));
    }

    #[test]
    fn validate_richtext_date_format() {
        let lua = Lua::new();
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("event", "Event")
                .attrs(vec![
                    FieldDefinition::builder("date", FieldType::Date).build(),
                ])
                .build(),
        );
        let field = make_richtext_field(vec!["event".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"event","attrs":{"date":"not-a-date"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("date"));
    }

    #[test]
    fn validate_richtext_custom_lua_validator() {
        let lua = Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                url_validator = function(value, ctx)
                    if not value:match("^/") then
                        return "URL must start with /"
                    end
                    return true
                end
            }
        "#,
        )
        .exec()
        .unwrap();

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("link", "Link")
                .attrs(vec![
                    FieldDefinition::builder("href", FieldType::Text)
                        .validate("validators.url_validator")
                        .build(),
                ])
                .build(),
        );
        let field = make_richtext_field(vec!["link".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"link","attrs":{"href":"example.com"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("URL must start with /"));
    }

    /// Regression: an ERRORING custom validator (broken ref, Lua error)
    /// was only warned about and the document saved — fail-open. It must
    /// reject the write like the top-level and sub-field paths do.
    #[test]
    fn validate_richtext_custom_lua_validator_error_fails_closed() {
        let lua = Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                broken = function(value, ctx)
                    error("validator exploded")
                end
            }
        "#,
        )
        .exec()
        .unwrap();

        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("link", "Link")
                .attrs(vec![
                    FieldDefinition::builder("href", FieldType::Text)
                        .validate("validators.broken")
                        .build(),
                ])
                .build(),
        );
        let field = make_richtext_field(vec!["link".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"link","attrs":{"href":"/x"}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(
            errors.len(),
            1,
            "an erroring validator must produce a validation error, not pass"
        );
        assert_eq!(errors[0].key.as_deref(), Some("validation.custom_error"));
    }

    // --- Additional edge case tests ---

    #[test]
    fn validate_richtext_max_length_violation() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        // text has max_length=100 — create a string that exceeds it
        let long_text = "a".repeat(101);
        let json = format!(
            r#"{{"type":"doc","content":[{{"type":"cta","attrs":{{"text":"{long_text}","url":"/ok"}}}}]}}"#
        );
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json.as_str()),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 1);
        assert!(errors[0].field.contains("content[cta#0].text"));
        assert!(errors[0].message.contains("100") || errors[0].message.contains("characters"));
    }

    #[test]
    fn validate_richtext_node_with_no_attrs_in_content() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        // Node exists but has no attrs object
        let json = r#"{"type":"doc","content":[{"type":"cta"}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        // text and url are both required, missing attrs = all empty
        assert_eq!(errors.len(), 2);
        assert!(errors[0].field.contains("content[cta#0].text"));
        assert!(errors[1].field.contains("content[cta#0].url"));
    }

    #[test]
    fn validate_richtext_multiple_nodes_error_indexing() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        // Two CTA nodes, both with missing required fields
        let json = r#"{"type":"doc","content":[
            {"type":"cta","attrs":{"text":"","url":""}},
            {"type":"paragraph","content":[{"type":"text","text":"sep"}]},
            {"type":"cta","attrs":{"text":"","url":""}}
        ]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert_eq!(errors.len(), 4);
        assert!(errors[0].field.contains("cta#0"));
        assert!(errors[1].field.contains("cta#0"));
        assert!(errors[2].field.contains("cta#1"));
        assert!(errors[3].field.contains("cta#1"));
    }

    #[test]
    fn validate_richtext_draft_skips_required() {
        let lua = Lua::new();
        let reg = make_registry_with_cta();
        let field = make_richtext_field(vec!["cta".to_string()], "json");
        let json = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"","url":""}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages")
                .draft(true)
                .build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert!(
            errors.is_empty(),
            "draft mode should skip required check on node attrs"
        );
    }

    #[test]
    fn validate_richtext_checkbox_type() {
        let lua = Lua::new();
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("toggle", "Toggle")
                .attrs(vec![
                    FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
                ])
                .build(),
        );
        let field = make_richtext_field(vec!["toggle".to_string()], "json");
        // Checkbox with boolean value — should pass without errors
        let json = r#"{"type":"doc","content":[{"type":"toggle","attrs":{"enabled":true}}]}"#;
        let mut errors = Vec::new();

        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(&lua, &reg, "pages").build(),
            &Value::from(json),
            "content",
            &field,
            &mut errors,
        );

        assert!(errors.is_empty(), "checkbox with boolean value should pass");
    }
}
