//! Rich text custom-node attrs: each node's attrs are validated against the
//! registered node definition.

use serde_json::Value;

use crate::{
    core::{FieldDefinition, FieldType, validate::FieldError},
    db::LocaleContext,
    hooks::lifecycle::validation::richtext_attrs::{
        RichtextValidationCtx, validate_richtext_node_attrs,
    },
};

use super::dispatch::ValidationWalker;

impl ValidationWalker<'_> {
    /// Validate custom-node attrs within a `Richtext` field's content.
    /// No-op for non-richtext fields, empty values, or fields without
    /// custom nodes registered.
    pub(super) fn validate_richtext_node_attrs_field(
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
        let (Some(registry), Some(content)) = (self.ctx.registry, value) else {
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
                .stored(Some(self.stored))
                .build(),
            content,
            data_key,
            field,
            errors,
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        core::{
            DocumentFields, FieldAdmin, FieldDefinition, FieldType, LocalizedString, Registry,
            RichtextNodeDef, SelectOption,
        },
        db::InMemoryConn,
        hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
    };

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

    /// A rich text `body` enabling the `alert` node, whose `style` select no
    /// longer offers `legacy`.
    fn alert_body() -> (FieldDefinition, Registry) {
        let mut reg = Registry::new();
        reg.register_richtext_node(
            RichtextNodeDef::builder("alert", "Alert")
                .attrs(vec![
                    FieldDefinition::builder("style", FieldType::Select)
                        .options(vec![SelectOption::new(
                            LocalizedString::Plain("Info".to_string()),
                            "info",
                        )])
                        .build(),
                ])
                .build(),
        );

        let body = FieldDefinition::builder("body", FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .nodes(vec!["alert".to_string()])
                    .richtext_format("json")
                    .build(),
            )
            .build();

        (body, reg)
    }

    fn alert(style: &str) -> Value {
        json!({ "type": "doc", "content": [
            { "type": "alert", "attrs": { "style": style } }
        ]})
    }

    /// The node attrs refused in a write submitting an alert styled `style` in
    /// the top-level `body` and in `items[].body` — both stored with `legacy`.
    fn refused_attrs(style: &str, update: bool) -> Vec<String> {
        let lua = mlua::Lua::new();
        let stored = alert("legacy").to_string();
        let conn = InMemoryConn::open();
        conn.setup(&format!(
            "CREATE TABLE pages (id TEXT PRIMARY KEY, body TEXT);
             CREATE TABLE pages_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, \
             body TEXT);
             INSERT INTO pages VALUES ('p1', '{stored}');
             INSERT INTO pages_items VALUES ('i1', 'p1', 0, '{stored}');"
        ));

        let (body, reg) = alert_body();
        let fields = vec![
            body.clone(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![body])
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("body".to_string(), alert(style));
        data.insert("items".to_string(), json!([{ "body": alert(style) }]));

        let ctx = ValidationCtx::builder(&conn, "pages")
            .registry(&reg)
            .exclude_id(update.then_some("p1"))
            .build();

        validate_fields_inner(&lua, &fields, &data, &ctx)
            .err()
            .map(|e| e.errors.into_iter().map(|fe| fe.field).collect())
            .unwrap_or_default()
    }

    /// Regression: a custom node attr option retired since the document was
    /// written blocked every later save of it. The value the document holds on
    /// the same attr of the same node type passes, at the top level and in a
    /// row; a newly chosen one, or any on a create, is still refused.
    #[test]
    fn a_held_retired_node_attr_option_passes_unchanged() {
        assert!(refused_attrs("legacy", true).is_empty());

        assert_eq!(
            refused_attrs("invented", true),
            vec!["body[alert#0].style", "items[0][body][alert#0].style"]
        );
        assert_eq!(refused_attrs("legacy", false).len(), 2);
    }
}
