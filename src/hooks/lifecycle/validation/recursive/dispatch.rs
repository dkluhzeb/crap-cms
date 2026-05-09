//! Layout dispatch: walk through Group/Row/Collapsible/Tabs containers and
//! delegate scalar fields to `ValidationWalker::scalar`.

use mlua::Lua;

use crate::{
    core::{DocumentFields, FieldDefinition, FieldType, validate::FieldError},
    db::query::helpers::prefixed_name,
    hooks::ValidationCtx,
};

/// Per-walk invariants for recursive validation. Methods take ≤ 4 args.
pub(in crate::hooks::lifecycle::validation) struct ValidationWalker<'a> {
    pub(in crate::hooks::lifecycle::validation::recursive) lua: &'a Lua,
    pub(in crate::hooks::lifecycle::validation::recursive) data: &'a DocumentFields,
    pub(in crate::hooks::lifecycle::validation::recursive) ctx: &'a ValidationCtx<'a>,
}

impl<'a> ValidationWalker<'a> {
    pub(in crate::hooks::lifecycle::validation) fn new(
        lua: &'a Lua,
        data: &'a DocumentFields,
        ctx: &'a ValidationCtx<'a>,
    ) -> Self {
        Self { lua, data, ctx }
    }

    /// Recursive validation with prefix support for arbitrary nesting.
    /// Group accumulates prefix (`group__`), Row/Collapsible/Tabs pass through.
    /// `inherited_localized` tracks locale state for unique checks.
    pub(in crate::hooks::lifecycle::validation) fn walk(
        &self,
        fields: &[FieldDefinition],
        prefix: &str,
        inherited_localized: bool,
        errors: &mut Vec<FieldError>,
    ) {
        for field in fields {
            match field.field_type {
                FieldType::Group => {
                    let new_prefix = prefixed_name(prefix, &field.name);
                    self.walk(
                        &field.fields,
                        &new_prefix,
                        inherited_localized || field.localized,
                        errors,
                    );
                }
                FieldType::Row | FieldType::Collapsible => {
                    self.walk(&field.fields, prefix, inherited_localized, errors);
                }
                FieldType::Tabs => {
                    for tab in &field.tabs {
                        self.walk(&tab.fields, prefix, inherited_localized, errors);
                    }
                }
                FieldType::Join => {
                    // Virtual field — no data to validate
                }
                _ => {
                    self.scalar(field, prefix, inherited_localized, errors);
                }
            }
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::DocumentFields;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType, JoinConfig};
    use crate::db::InMemoryConn;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;

    #[test]
    fn test_validate_group_subfield_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_required_inside_collapsible() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, notes TEXT)");
        let fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("notes", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("notes".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "notes");
    }

    #[test]
    fn test_validate_required_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .required(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "body");
    }

    #[test]
    fn test_validate_group_inside_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "SEO",
                    vec![
                        FieldDefinition::builder("seo", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("title", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_group_inside_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("seo", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_date_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, publish_date TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Meta",
                    vec![FieldDefinition::builder("publish_date", FieldType::Date).build()],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("publish_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_unique_inside_tabs() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('existing', 'taken');",
        );
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Meta",
                    vec![
                        FieldDefinition::builder("slug", FieldType::Text)
                            .unique(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("slug".to_string(), json!("taken"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_custom_function_inside_tabs() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_tabs_field = function(value, ctx)
    
                if value == "bad" then return "tabs validation error" end
    
                return true
            end
        "#,
        )
        .exec()
        .unwrap();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .validate("validators.validate_tabs_field")
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("body".to_string(), json!("bad"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("tabs validation error")
        );
    }

    #[test]
    fn test_validate_deeply_nested_tabs_collapsible_group() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, og__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Advanced",
                    vec![
                        FieldDefinition::builder("advanced", FieldType::Collapsible)
                            .fields(vec![
                                FieldDefinition::builder("og", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("title", FieldType::Text)
                                            .required(true)
                                            .build(),
                                    ])
                                    .build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("og__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Deeply nested Group inside Collapsible inside Tabs should validate"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "og__title");
    }

    #[test]
    fn test_validate_layout_fields_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![
                        FieldDefinition::builder("body", FieldType::Text)
                            .required(true)
                            .build(),
                    ],
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").draft(true).build(),
        );
        assert!(
            result.is_ok(),
            "Draft saves should skip required checks in layout fields"
        );
    }

    // ── Group containing layout fields ─────

    #[test]
    fn test_validate_group_containing_row_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("meta__title".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Group→Row: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__title");
    }

    #[test]
    fn test_validate_group_containing_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, seo__robots TEXT)");
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("c", FieldType::Collapsible)
                        .fields(vec![
                            FieldDefinition::builder("robots", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("seo__robots".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Group→Collapsible: required field should fail"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "seo__robots");
    }

    #[test]
    fn test_validate_group_containing_tabs_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, settings__theme TEXT)");
        let fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "General",
                            vec![
                                FieldDefinition::builder("theme", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("settings__theme".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Group→Tabs: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "settings__theme");
    }

    #[test]
    fn test_validate_group_tabs_group_three_levels_required() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__deep TEXT)");
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("inner", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("deep", FieldType::Text)
                                            .required(true)
                                            .build(),
                                    ])
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("outer__inner__deep".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Group→Tabs→Group: required field should fail"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__deep");
    }

    #[test]
    fn test_validate_group_containing_tabs_unique() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE test (id TEXT PRIMARY KEY, config__slug TEXT);
             INSERT INTO test (id, config__slug) VALUES ('existing', 'taken');",
        );
        let fields = vec![
            FieldDefinition::builder("config", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("slug", FieldType::Text)
                                    .unique(true)
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("config__slug".to_string(), json!("taken"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Group→Tabs: unique field should fail on duplicate"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "config__slug");
    }

    #[test]
    fn test_validate_group_containing_row_date_format() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, meta__date TEXT)");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("date", FieldType::Date).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("meta__date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Group→Row: invalid date should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__date");
    }

    #[test]
    fn test_validate_group_containing_row_valid_passes() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("meta__title".to_string(), json!("Valid Title"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Group→Row: valid data should pass");
    }

    #[test]
    fn join_field_skipped_in_validation() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY)");
        let fields = vec![
            FieldDefinition::builder("posts", FieldType::Join)
                .required(true)
                .join(JoinConfig::new("posts", "author"))
                .build(),
        ];
        let data = DocumentFields::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Join field should be skipped entirely during validation"
        );
    }

    #[test]
    fn test_validate_nested_group_in_group_prefix() {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__field TEXT)");
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("field", FieldType::Text)
                                .required(true)
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("outer__inner__field".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Nested group prefix should be outer__inner__field"
        );
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__field");
    }
}
