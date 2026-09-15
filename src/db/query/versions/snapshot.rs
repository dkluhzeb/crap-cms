//! Snapshot building and data extraction helpers.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    config::LocaleConfig,
    core::{
        Document, DocumentFields, FieldChildren, FieldDefinition, FieldType, field_children,
        flatten_group_fields, prefixed_name, walk_leaf_fields,
    },
    db::{
        DbConnection, DbValue,
        query::{
            LocaleContext, LocaleMode, get_locale_select_columns_full, helpers::locale_column,
            join::hydrate_document, per_locale_columns, read::decode_row,
        },
    },
};

/// Build a JSON snapshot of a document's current state (fields + join data).
///
/// # Errors
///
/// Returns a backend error if hydration of join-table data fails.
pub fn build_snapshot(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &Document,
    locale_config: Option<&LocaleConfig>,
) -> Result<Value> {
    let active_locales = locale_config.filter(|c| c.is_enabled());

    // With locales on, join fields hydrate under the default locale, so a
    // localized join field's bare key holds the default locale's rows — never a
    // mix of every locale's. Each locale's own rows are recorded below.
    let default_ctx = active_locales.map(|c| LocaleContext::exact(c, &c.default_locale));

    let mut hydrated = doc.clone();
    hydrate_document(
        conn,
        slug,
        fields,
        &mut hydrated,
        None,
        default_ctx.as_ref(),
    )?;

    let mut data: Map<String, Value> = hydrated.fields.into_iter().collect();

    // `doc` was resolved under ONE locale, so it carries a single value per
    // localized field. A snapshot must hold every locale's column: restore
    // writes the decorated `field__xx` columns back, and anything missing
    // there is written as NULL — losing the other locales' translations.
    add_locale_columns(conn, slug, fields, &doc.id, locale_config, &mut data)?;

    // Each locale's rows of a localized join field under the key its column
    // would have (`{key}__{locale}`, the locale code in column form): restore
    // writes each locale back from its own key, and a draft read resolves the
    // reading locale's rows from it.
    if let Some(config) = active_locales {
        for (key, by_locale) in locale_join_rows(conn, doc, JoinOwner::new(slug, fields), config)? {
            for (locale, rows) in by_locale {
                data.insert(locale_column(&key, &locale)?, rows);
            }
        }
    }

    if let Some(ts) = &doc.created_at {
        data.insert("created_at".to_string(), Value::String(ts.clone()));
    }
    if let Some(ts) = &doc.updated_at {
        data.insert("updated_at".to_string(), Value::String(ts.clone()));
    }

    Ok(Value::Object(data))
}

/// The collection a document belongs to: its slug and fields.
#[derive(Clone, Copy)]
pub(crate) struct JoinOwner<'a> {
    pub slug: &'a str,
    pub fields: &'a [FieldDefinition],
}

impl<'a> JoinOwner<'a> {
    #[must_use]
    pub(crate) fn new(slug: &'a str, fields: &'a [FieldDefinition]) -> Self {
        Self { slug, fields }
    }
}

/// Flat keys (`field`, `group__field`) of the localized join fields — array,
/// blocks and has-many relationship fields whose rows are stored per locale in
/// a join table. Join localization follows the field's own `localized` flag.
pub(crate) fn localized_join_keys(fields: &[FieldDefinition]) -> Vec<String> {
    let mut keys = Vec::new();

    let _ = walk_leaf_fields(fields, "", false, &mut |field, prefix, _| {
        if field.localized && !field.has_parent_column() && field.field_type != FieldType::Join {
            keys.push(prefixed_name(prefix, &field.name));
        }

        Ok(())
    });

    keys
}

/// Every locale's rows of each localized join field of `doc` — the join-table
/// counterpart of [`add_locale_columns`] — keyed by the field's flat key
/// (`field`, `group__field`) and then by locale code. `owner` is the collection
/// `doc` belongs to. Records exactly the rows each locale holds, never
/// the default locale's standing in for an empty one.
///
/// # Errors
///
/// Returns a backend error if hydrating a locale's rows fails.
pub(crate) fn locale_join_rows(
    conn: &dyn DbConnection,
    doc: &Document,
    owner: JoinOwner<'_>,
    config: &LocaleConfig,
) -> Result<Vec<(String, Map<String, Value>)>> {
    let JoinOwner { slug, fields } = owner;
    let mut rows: Vec<(String, Map<String, Value>)> = localized_join_keys(fields)
        .into_iter()
        .map(|key| (key, Map::new()))
        .collect();
    if rows.is_empty() {
        return Ok(rows);
    }

    for locale in &config.locales {
        let mut per_locale = doc.clone();
        let ctx = LocaleContext::exact(config, locale);
        hydrate_document(conn, slug, fields, &mut per_locale, None, Some(&ctx))?;

        let flat = flatten_group_fields(&per_locale.fields, fields);
        for (key, by_locale) in &mut rows {
            if let Some(locale_rows) = flat.get(key) {
                by_locale.insert(locale.clone(), locale_rows.clone());
            }
        }
    }

    Ok(rows)
}

/// Read the per-locale columns (`title__en`, `title__de`, …) of one row and
/// merge them into `data` under their decorated names, typed as the published
/// read types them. No-op when localization is disabled or the collection has
/// no localized field.
fn add_locale_columns(
    conn: &dyn DbConnection,
    slug: &str,
    fields: &[FieldDefinition],
    id: &str,
    locale_config: Option<&LocaleConfig>,
    data: &mut Map<String, Value>,
) -> Result<()> {
    let Some(config) = locale_config.filter(|c| c.is_enabled()) else {
        return Ok(());
    };

    let ctx = LocaleContext {
        mode: LocaleMode::All,
        config: config.clone(),
    };
    // Only per-locale columns are recorded under their own keys; a group's plain
    // column (`seo__title`) is already in the nested group.
    let per_locale = per_locale_columns(fields, config)?;
    if per_locale.is_empty() {
        return Ok(());
    }

    let (exprs, _) = get_locale_select_columns_full(fields, false, false, false, &ctx)?;

    let sql = format!(
        "SELECT {} FROM \"{slug}\" WHERE id = {}",
        exprs.join(", "),
        conn.placeholder(1)
    );
    let Some(row) = conn.query_one(&sql, &[DbValue::Text(id.to_string())])? else {
        return Ok(());
    };

    // Decoded as every read decodes a row.
    for (name, value) in decode_row(conn, &row, fields, None)?.fields {
        if per_locale.contains(&name) {
            data.insert(name, value);
        }
    }

    Ok(())
}

/// Whether a snapshot JSON value is a scalar that maps to a column write.
/// Arrays / objects are handled via join tables and skipped here — except a
/// scalar has-many list, which its own column stores.
fn is_scalar_snapshot_value(val: &Value) -> bool {
    !matches!(val, Value::Array(_) | Value::Object(_))
}

/// Extract flat field data from a snapshot for the UPDATE statement (version
/// restore). Group fields are expanded to `field__subfield` sub-columns.
///
/// Snapshots are stored nested (`build_snapshot` hydrates groups), but legacy
/// snapshots written before group hydration may be flat. [`flatten_group_fields`]
/// accepts either shape and yields the canonical flat `group__sub` columns
/// (idempotent), so extraction is a single flat-column pass over the schema via
/// [`walk_leaf_fields`] — no bespoke nested/flat merge. Only scalar, non-localized
/// columns that live on the parent row are taken — a scalar has-many list among
/// them; localized columns (separate locale columns), join-table data
/// (arrays/blocks/has-many relationships), and
/// `created_at`/`updated_at` are handled elsewhere. Companion columns (`_tz`,
/// `_lang`) ride along with their owning column.
pub(super) fn extract_snapshot_data(
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    locales_enabled: bool,
) -> DocumentFields {
    let as_fields: DocumentFields = obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let flat = flatten_group_fields(&as_fields, fields);

    let mut data = DocumentFields::new();

    let _ = walk_leaf_fields(
        fields,
        "",
        false,
        &mut |field, prefix, inherited_localized| {
            if !field.has_parent_column() {
                return Ok(());
            }

            if (inherited_localized || field.localized) && locales_enabled {
                return Ok(());
            }

            let key = prefixed_name(prefix, &field.name);

            if let Some(val) = flat.get(&key)
                && (is_scalar_snapshot_value(val) || field.is_has_many_scalar())
            {
                data.insert(key.clone(), val.clone());
            }

            for companion in field.companion_columns(&key) {
                if let Some(val) = flat.get(&companion)
                    && is_scalar_snapshot_value(val)
                {
                    data.insert(companion, val.clone());
                }
            }

            Ok(())
        },
    );

    data
}

/// Recursively collect join table data (Blocks/Arrays/Relationships) from a snapshot,
/// including fields nested inside Tabs/Row/Collapsible layout wrappers.
pub(super) fn collect_join_data_from_snapshot(
    fields: &[FieldDefinition],
    obj: &Map<String, Value>,
    join_data: &mut DocumentFields,
) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Wrapper(sub) => {
                collect_join_data_from_snapshot(sub, obj, join_data);
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_join_data_from_snapshot(&tab.fields, obj, join_data);
                }
            }
            // Group is deliberately NOT descended: its join-bearing sub-fields
            // are captured under their prefixed keys elsewhere, and at this
            // level the group value is captured by name like any non-parent-
            // column field — the same path Array/Blocks/Relationship and
            // scalars take.
            FieldChildren::Group(_)
            | FieldChildren::Array(_)
            | FieldChildren::Blocks(_)
            | FieldChildren::Leaf => {
                if !field.has_parent_column()
                    && let Some(v) = obj.get(&field.name)
                {
                    join_data.insert(field.name.clone(), v.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::{CrapConfig, LocaleConfig},
        core::{
            Document, DocumentFields, FieldAdmin, FieldDefinition, FieldTab, RelationshipConfig,
        },
        db::{BoxedConnection, DbConnection, pool},
    };

    fn setup_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let db_pool = pool::create_pool(dir.path(), &CrapConfig::default()).unwrap();
        let conn = db_pool.get().unwrap();
        (dir, conn)
    }

    fn en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn code_lang_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["javascript".to_string(), "python".to_string()])
                    .build(),
            )
            .build()
    }

    #[test]
    fn extract_snapshot_data_basic() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"title": "Hello", "count": 42})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("title"), Some(&json!("Hello")));
        assert_eq!(data.get("count"), Some(&json!(42)));
    }

    /// Regression: extraction skipped every array value, so restoring a version
    /// left a scalar has-many list at its current value.
    #[test]
    fn extract_snapshot_data_keeps_a_scalar_has_many_list() {
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({"tags": ["a", "b"]})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("tags"), Some(&json!(["a", "b"])));
    }

    #[test]
    fn extract_snapshot_data_skips_localized_when_enabled() {
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"title": "Hello", "slug": "hello"})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, true);
        assert!(
            !data.contains_key("title"),
            "localized field should be skipped"
        );
        assert_eq!(data.get("slug"), Some(&json!("hello")));
    }

    #[test]
    fn extract_snapshot_data_group_fields() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        // Flat format: seo__title
        let obj: Map<String, Value> =
            serde_json::from_value(json!({"seo__title": "SEO Title"})).unwrap();
        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("seo__title"), Some(&json!("SEO Title")));

        // Nested format: seo: { title: "..." }
        let obj2: Map<String, Value> =
            serde_json::from_value(json!({"seo": {"title": "Nested SEO"}})).unwrap();
        let data2 = extract_snapshot_data(&obj2, &fields, false);
        assert_eq!(data2.get("seo__title"), Some(&json!("Nested SEO")));
    }

    #[test]
    fn extract_snapshot_data_tabs_promotes_sub_fields() {
        // Fields inside Tabs should be promoted as top-level columns (no prefix)
        let fields = vec![
            FieldDefinition::builder("page_settings", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Settings",
                    vec![
                        FieldDefinition::builder("template", FieldType::Select).build(),
                        FieldDefinition::builder("show_in_nav", FieldType::Checkbox).build(),
                    ],
                )])
                .build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"template": "landing", "show_in_nav": true})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("template"), Some(&json!("landing")));
        assert_eq!(data.get("show_in_nav"), Some(&json!(true)));
    }

    #[test]
    fn extract_snapshot_data_row_promotes_sub_fields() {
        let fields = vec![
            FieldDefinition::builder("main_row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("width", FieldType::Number).build(),
                ])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({"width": 100})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(data.get("width"), Some(&json!(100)));
    }

    #[test]
    fn extract_snapshot_data_nested_row_in_tabs() {
        // Regression: Row inside Tabs at the collection top level was not recursed
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "General",
                    vec![
                        FieldDefinition::builder("inner_row", FieldType::Row)
                            .fields(vec![
                                FieldDefinition::builder("title", FieldType::Text).build(),
                                FieldDefinition::builder("slug", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];

        let obj: Map<String, Value> =
            serde_json::from_value(json!({"title": "Hello", "slug": "hello"})).unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);
        assert_eq!(
            data.get("title"),
            Some(&json!("Hello")),
            "Row inside Tabs must be recursed"
        );
        assert_eq!(data.get("slug"), Some(&json!("hello")));
    }

    #[test]
    fn collect_join_data_from_snapshot_tabs() {
        // Blocks inside Tabs should be collected as join data
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("page_settings", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Content",
                    vec![FieldDefinition::builder("content", FieldType::Blocks).build()],
                )])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "title": "Hello",
            "content": [{"_block_type": "hero", "heading": "Welcome"}]
        }))
        .unwrap();

        let mut join_data = DocumentFields::new();
        collect_join_data_from_snapshot(&fields, &obj, &mut join_data);

        assert!(
            !join_data.contains_key("title"),
            "scalar field should not be in join data"
        );
        assert!(
            join_data.contains_key("content"),
            "blocks inside Tabs must be in join data"
        );
        let blocks = join_data["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["_block_type"], "hero");
    }

    #[test]
    fn collect_join_data_from_snapshot_row_and_collapsible() {
        let fields = vec![
            FieldDefinition::builder("row_wrapper", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("items", FieldType::Array).build(),
                ])
                .build(),
            FieldDefinition::builder("advanced", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("related", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("tags", true))
                        .build(),
                ])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "items": [{"label": "A"}],
            "related": ["t1", "t2"]
        }))
        .unwrap();

        let mut join_data = DocumentFields::new();
        collect_join_data_from_snapshot(&fields, &obj, &mut join_data);

        assert!(
            join_data.contains_key("items"),
            "array inside Row must be in join data"
        );
        assert!(
            join_data.contains_key("related"),
            "has-many inside Collapsible must be in join data"
        );
    }

    // ── Timezone companion column tests ──────────────────────────────

    #[test]
    fn extract_snapshot_data_includes_tz_companion() {
        // Regression: extract_snapshot_data must extract _tz companion columns
        // for Date fields with timezone: true, so version restore works.
        let fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "start_date": "2024-06-15T14:00:00.000Z",
            "start_date_tz": "America/New_York"
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(
            data.get("start_date"),
            Some(&json!("2024-06-15T14:00:00.000Z")),
            "Date value should be extracted"
        );
        assert_eq!(
            data.get("start_date_tz"),
            Some(&json!("America/New_York")),
            "Timezone companion should be extracted"
        );
    }

    #[test]
    fn extract_snapshot_data_date_without_tz_no_companion() {
        // Date field without timezone: true should NOT extract a _tz column.
        let fields = vec![FieldDefinition::builder("event_date", FieldType::Date).build()];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "event_date": "2024-06-15T14:00:00.000Z"
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(
            data.get("event_date"),
            Some(&json!("2024-06-15T14:00:00.000Z"))
        );
        assert!(
            !data.contains_key("event_date_tz"),
            "No _tz column should be extracted for non-timezone date"
        );
    }

    #[test]
    fn extract_snapshot_data_group_date_tz_companion() {
        // Date field with timezone inside a Group: the _tz companion should
        // be extracted with the group prefix.
        let fields = vec![
            FieldDefinition::builder("schedule", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("start", FieldType::Date)
                        .timezone(true)
                        .build(),
                ])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "schedule__start": "2024-06-15T07:00:00.000Z",
            "schedule__start_tz": "Europe/Berlin"
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(
            data.get("schedule__start"),
            Some(&json!("2024-06-15T07:00:00.000Z"))
        );
        assert_eq!(
            data.get("schedule__start_tz"),
            Some(&json!("Europe/Berlin")),
            "Group _tz companion should be extracted with prefix"
        );
    }

    // ── Code language companion column tests ─────────────────────────

    /// Regression: restore extraction took only `_tz` companions, so restoring
    /// a version left a code field's language at its current value.
    #[test]
    fn extract_snapshot_data_includes_code_lang_companion() {
        let fields = vec![code_lang_field("snippet")];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "snippet": "print(1)",
            "snippet_lang": "python"
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(data.get("snippet"), Some(&json!("print(1)")));
        assert_eq!(
            data.get("snippet_lang"),
            Some(&json!("python")),
            "Language companion should be extracted"
        );
    }

    #[test]
    fn extract_snapshot_data_group_code_lang_companion() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![code_lang_field("example")])
                .build(),
        ];

        let obj: Map<String, Value> = serde_json::from_value(json!({
            "meta": { "example": "console.log(1)", "example_lang": "javascript" }
        }))
        .unwrap();

        let data = extract_snapshot_data(&obj, &fields, false);

        assert_eq!(
            data.get("meta__example_lang"),
            Some(&json!("javascript")),
            "Group _lang companion should be extracted with prefix"
        );
    }

    /// Regression: the snapshot build copied every column whose name holds `__`
    /// as a per-locale column — a plain group column (`seo__title`) too. It sat
    /// flat beside the nested group, and flattening the snapshot picked either
    /// value at random, so a first draft save could record the published value
    /// instead of the edit.
    #[test]
    fn a_snapshot_keeps_only_per_locale_columns_flat() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE items (
                id TEXT PRIMARY KEY,
                title__en TEXT,
                title__de TEXT,
                seo__title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO items (id, title__en, seo__title) VALUES ('i1', 'Hello', 'published');",
        )
        .unwrap();
        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        };
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];
        let mut group = DocumentFields::new();
        group.insert("seo".to_string(), json!({ "title": "edited" }));
        let doc = Document::builder("i1").fields(group).build();

        let snapshot = build_snapshot(&conn, "items", &fields, &doc, Some(&locale)).unwrap();

        assert_eq!(snapshot.get("seo__title"), None);
        assert_eq!(snapshot["seo"], json!({ "title": "edited" }));
        assert_eq!(snapshot["title__en"], json!("Hello"));
    }

    /// Regression: the snapshot build recorded no per-locale `_lang` columns,
    /// so a version lost every non-default locale's language pick.
    #[test]
    fn a_snapshot_records_every_locales_code_language() {
        let (_dir, conn) = setup_conn();
        conn.execute_batch(
            "CREATE TABLE snippets (
                id TEXT PRIMARY KEY,
                snippet__en TEXT,
                snippet__de TEXT,
                snippet_lang__en TEXT,
                snippet_lang__de TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO snippets (id, snippet__en, snippet__de, snippet_lang__en, snippet_lang__de)
                VALUES ('s1', 'console.log(1)', 'print(1)', 'javascript', 'python');",
        )
        .unwrap();

        let mut snippet = code_lang_field("snippet");
        snippet.localized = true;
        let fields = vec![snippet];
        let doc = Document::builder("s1").build();

        let snapshot = build_snapshot(&conn, "snippets", &fields, &doc, Some(&en_de())).unwrap();

        assert_eq!(snapshot["snippet_lang__en"], json!("javascript"));
        assert_eq!(snapshot["snippet_lang__de"], json!("python"));
    }
}
