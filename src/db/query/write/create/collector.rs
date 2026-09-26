//! INSERT column collection: the accumulator and the recursive field walk.

use anyhow::Result;

use crate::{
    core::{DocumentFields, FieldDefinition, flatten_group_fields},
    db::{
        DbConnection, DbValue, LocaleContext,
        query::{
            helpers::{quote_ident, walk_leaf_fields},
            write::create::leaf::collect_leaf_param,
        },
    },
};

/// Accumulator for INSERT column/placeholder/param collection during recursive field traversal.
pub(super) struct InsertCollector {
    pub columns: Vec<String>,
    pub placeholders: Vec<String>,
    pub params: Vec<DbValue>,
    idx: usize,
}

impl InsertCollector {
    /// A collector holding the row's `id` — the first column of every INSERT,
    /// and the first bound parameter. Seeding it here keeps the placeholder
    /// numbering entirely inside [`InsertCollector::push`], so no caller has to
    /// know which index the columns it adds start at.
    pub(super) fn new(conn: &dyn DbConnection, id: &str) -> Self {
        let mut collector = Self {
            columns: Vec::new(),
            placeholders: Vec::new(),
            params: Vec::new(),
            idx: 1,
        };

        collector.push(conn, "id", DbValue::Text(id.to_string()));

        collector
    }

    /// Push a column, its placeholder, and value.
    pub(super) fn push(&mut self, conn: &dyn DbConnection, col: &str, val: DbValue) {
        self.columns.push(quote_ident(col));
        self.placeholders.push(conn.placeholder(self.idx));
        self.params.push(val);
        self.idx += 1;
    }
}

/// Collect columns, placeholders, and params for INSERT.
/// Uses `walk_leaf_fields` to handle Group/Row/Collapsible/Tabs recursion.
pub(super) fn collect_insert_params(
    fields: &[FieldDefinition],
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
    collector: &mut InsertCollector,
    conn: &dyn DbConnection,
) -> Result<()> {
    // The persistence edge owns the flat `group__sub` column encoding (nested is
    // the canonical in-memory shape everywhere above `db/query`). Flatten here —
    // idempotent, the mirror of read-side group hydration.
    let data = flatten_group_fields(data, fields);

    walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if field.has_parent_column() {
                collect_leaf_param(
                    field,
                    &data,
                    locale_ctx,
                    collector,
                    conn,
                    prefix,
                    inherited_localized,
                )?;
            }

            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldTab, FieldType},
        db::query::write::create::{create, test_support::setup_db},
    };

    #[test]
    fn create_with_group_fields() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                meta__color TEXT,
                meta__size TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("color", FieldType::Text).build(),
                    FieldDefinition::builder("size", FieldType::Text).build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Post1"));
        data.insert("meta__color".to_string(), json!("red"));
        data.insert("meta__size".to_string(), json!("large"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("title"), Some("Post1"));
        // Group sub-fields stored as prefixed columns
        assert_eq!(doc.get_str("meta__color"), Some("red"));
        assert_eq!(doc.get_str("meta__size"), Some("large"));
    }

    #[test]
    fn create_with_collapsible_fields() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                notes TEXT,
                footer TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("notes", FieldType::Text).build(),
                    FieldDefinition::builder("footer", FieldType::Text).build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("notes".to_string(), json!("Some notes"));
        data.insert("footer".to_string(), json!("Copyright"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("notes"), Some("Some notes"));
        assert_eq!(doc.get_str("footer"), Some("Copyright"));
    }

    #[test]
    fn create_with_tabs_fields() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                body TEXT,
                slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "Content",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    ),
                    FieldTab::new(
                        "Meta",
                        vec![FieldDefinition::builder("slug", FieldType::Text).build()],
                    ),
                ])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("body".to_string(), json!("Hello world"));
        data.insert("slug".to_string(), json!("hello-world"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("body"), Some("Hello world"));
        assert_eq!(doc.get_str("slug"), Some("hello-world"));
    }

    #[test]
    fn create_with_tabs_containing_group() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                social__github TEXT,
                social__twitter TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "Social",
                        vec![
                            FieldDefinition::builder("social", FieldType::Group)
                                .fields(vec![
                                    FieldDefinition::builder("github", FieldType::Text).build(),
                                    FieldDefinition::builder("twitter", FieldType::Text).build(),
                                ])
                                .build(),
                        ],
                    ),
                    FieldTab::new(
                        "Content",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    ),
                ])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("social__github".to_string(), json!("https://github.com"));
        data.insert("social__twitter".to_string(), json!("@test"));
        data.insert("body".to_string(), json!("Content here"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("social__github"), Some("https://github.com"));
        assert_eq!(doc.get_str("social__twitter"), Some("@test"));
        assert_eq!(doc.get_str("body"), Some("Content here"));
    }

    #[test]
    fn create_deeply_nested_tabs_collapsible_group() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                og__image TEXT,
                canonical TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Advanced",
                    vec![
                        FieldDefinition::builder("advanced", FieldType::Collapsible)
                            .fields(vec![
                                FieldDefinition::builder("og", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("image", FieldType::Text).build(),
                                    ])
                                    .build(),
                                FieldDefinition::builder("canonical", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("og__image".to_string(), json!("hero.jpg"));
        data.insert("canonical".to_string(), json!("https://example.com"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("og__image"), Some("hero.jpg"));
        assert_eq!(doc.get_str("canonical"), Some("https://example.com"));
    }

    #[test]
    fn create_group_containing_row() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                meta__title TEXT,
                meta__slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text).build(),
                            FieldDefinition::builder("slug", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("meta__title".to_string(), json!("Hello"));
        data.insert("meta__slug".to_string(), json!("hello"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("meta__title"), Some("Hello"));
        assert_eq!(doc.get_str("meta__slug"), Some("hello"));
    }

    #[test]
    fn create_group_containing_tabs() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                settings__theme TEXT,
                settings__cache_ttl TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("layout", FieldType::Tabs)
                        .tabs(vec![
                            FieldTab::new(
                                "General",
                                vec![FieldDefinition::builder("theme", FieldType::Text).build()],
                            ),
                            FieldTab::new(
                                "Advanced",
                                vec![
                                    FieldDefinition::builder("cache_ttl", FieldType::Text).build(),
                                ],
                            ),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("settings__theme".to_string(), json!("dark"));
        data.insert("settings__cache_ttl".to_string(), json!("3600"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("settings__theme"), Some("dark"));
        assert_eq!(doc.get_str("settings__cache_ttl"), Some("3600"));
    }

    #[test]
    fn create_group_tabs_group_three_levels() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                outer__inner__deep TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("inner", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("deep", FieldType::Text).build(),
                                    ])
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = DocumentFields::new();
        data.insert("outer__inner__deep".to_string(), json!("bottom"));

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("outer__inner__deep"), Some("bottom"));
    }
}
