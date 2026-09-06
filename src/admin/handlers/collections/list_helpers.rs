//! Pure list-view helpers — columns, cells, filter pills, column picker.
//!
//! These are data-transformation functions for the collection list page.
//! No async, no DB, no HTTP — all take definitions + documents and return JSON.

use crate::{
    admin::handlers::shared::{
        ListUrlContext, auto_label_from_name, is_column_eligible, is_sortable_column, url_decode,
    },
    core::{FieldDefinition, FieldType, collection::CollectionDefinition, document::Document},
    db::query::{FilterClause, FilterOp},
};

use serde_json::{Value, json};

/// Get the display label for a field (admin label or auto-generated from name).
pub(super) fn field_label(field: &FieldDefinition) -> String {
    if let Some(ref label) = field.admin.label {
        label.resolve_default().to_string()
    } else {
        auto_label_from_name(&field.name)
    }
}

/// Resolve which columns to display in the list table.
pub(super) fn resolve_columns(
    def: &CollectionDefinition,
    user_cols: Option<&[String]>,
    url_ctx: &ListUrlContext,
) -> Vec<Value> {
    // Keep only columns that exist: a meta column or an eligible field.
    let valid = |cols: &[String]| -> Vec<String> {
        cols.iter()
            .filter(|k| {
                matches!(k.as_str(), "created_at" | "updated_at" | "_status")
                    || def
                        .fields
                        .iter()
                        .any(|f| f.name == **k && is_column_eligible(&f.field_type))
            })
            .cloned()
            .collect()
    };

    // Precedence: a per-user column selection wins; then the collection's
    // configured `admin.list_columns` default; then the built-in fallback.
    let mut keys: Vec<String> = if let Some(cols) = user_cols {
        valid(cols)
    } else if !def.admin.list_columns.is_empty() {
        valid(&def.admin.list_columns)
    } else {
        let mut defaults = Vec::new();

        if def.has_drafts() {
            defaults.push("_status".to_string());
        }

        defaults.push("created_at".to_string());
        defaults
    };
    if let Some(title) = def.title_field() {
        keys.retain(|k| k != title);
    }

    let sort_field = url_ctx.sort.map(|s| s.strip_prefix('-').unwrap_or(s));
    let sort_desc = url_ctx.sort.is_some_and(|s| s.starts_with('-'));

    keys.iter()
        .map(|key| {
            let (label, sortable) = match key.as_str() {
                "created_at" => ("created".to_string(), true),
                "updated_at" => ("updated".to_string(), true),
                "_status" => ("status".to_string(), true),
                _ => {
                    if let Some(f) = def.fields.iter().find(|f| f.name == *key) {
                        // Sortability must match `validate_sort` exactly —
                        // a has-many relationship is a valid column but not
                        // sortable (no parent column); a sort header for it
                        // would 400 on click.
                        (field_label(f), is_sortable_column(key, def))
                    } else {
                        (auto_label_from_name(key), false)
                    }
                }
            };

            let is_sorted = sort_field == Some(key.as_str());
            let next_sort = if is_sorted && !sort_desc {
                format!("-{key}")
            } else {
                key.clone()
            };
            let sort_url = url_ctx.sort_url(&next_sort);

            json!({
                "key": key,
                "label": label,
                "sortable": sortable,
                "sort_url": sort_url,
                "is_sorted_asc": is_sorted && !sort_desc,
                "is_sorted_desc": is_sorted && sort_desc,
            })
        })
        .collect()
}

/// Pre-compute cell values for a document row, parallel to the columns array.
pub(super) fn compute_cells(
    doc: &Document,
    columns: &[Value],
    def: &CollectionDefinition,
) -> Vec<Value> {
    columns
        .iter()
        .map(|col| {
            let key = col["key"].as_str().unwrap_or("");
            match key {
                "_status" => {
                    let status = doc
                        .fields
                        .get("_status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("published");

                    json!({ "value": status, "is_badge": true })
                }
                "created_at" => {
                    json!({ "value": doc.created_at, "is_date": true })
                }
                "updated_at" => {
                    json!({ "value": doc.updated_at, "is_date": true })
                }
                _ => {
                    let field_def = def.fields.iter().find(|f| f.name == key);
                    let raw = doc.fields.get(key).cloned().unwrap_or(Value::Null);

                    if let Some(f) = field_def {
                        match f.field_type {
                            FieldType::Checkbox => {
                                let checked = match &raw {
                                    Value::Bool(b) => *b,
                                    Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
                                    _ => false,
                                };

                                json!({ "value": checked, "is_bool": true })
                            }
                            FieldType::Date => {
                                let val = raw.as_str().unwrap_or("");

                                json!({ "value": val, "is_date": true })
                            }
                            FieldType::Select | FieldType::Radio => {
                                let raw_val = raw.as_str().unwrap_or("");
                                let label =
                                    f.options.iter().find(|o| o.value == raw_val).map_or_else(
                                        || raw_val.to_string(),
                                        |o| o.label.resolve_default().to_string(),
                                    );

                                json!({ "value": label })
                            }
                            FieldType::Textarea => {
                                let text = raw.as_str().unwrap_or("");
                                let truncated = match text.char_indices().nth(80) {
                                    Some((i, _)) => format!("{}…", &text[..i]),
                                    None => text.to_string(),
                                };

                                json!({ "value": truncated })
                            }
                            FieldType::Relationship | FieldType::Upload => {
                                // List columns render raw (un-populated) field
                                // data, so we don't have target labels here
                                // (resolving them would be an N+1 per cell —
                                // deliberately out of scope). Render a clean
                                // value rather than raw JSON: the id for a
                                // has-one, an "N linked" summary for has-many
                                // (which stored a JSON array).
                                let value = match &raw {
                                    Value::String(s) => s.clone(),
                                    Value::Array(a) => format!("{} linked", a.len()),
                                    Value::Null => String::new(),
                                    other => other
                                        .as_str()
                                        .map_or_else(|| other.to_string(), str::to_string),
                                };

                                json!({ "value": value })
                            }
                            _ => {
                                let val = match &raw {
                                    Value::String(s) => s.clone(),
                                    Value::Number(n) => n.to_string(),
                                    Value::Bool(b) => b.to_string(),
                                    Value::Null => String::new(),
                                    other => other.to_string(),
                                };

                                json!({ "value": val })
                            }
                        }
                    } else {
                        let val = match &raw {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            other => other.to_string(),
                        };

                        json!({ "value": val })
                    }
                }
            }
        })
        .collect()
}

/// Build the list of all eligible columns for the column picker UI.
pub(super) fn build_column_options(
    def: &CollectionDefinition,
    selected_keys: &[String],
) -> Vec<Value> {
    let mut options = Vec::new();

    if def.has_drafts() {
        options.push(json!({
            "key": "_status",
            "label": "status",
            "selected": selected_keys.contains(&"_status".to_string()),
        }));
    }

    options.push(json!({
        "key": "created_at",
        "label": "created",
        "selected": selected_keys.contains(&"created_at".to_string()),
    }));

    options.push(json!({
        "key": "updated_at",
        "label": "updated",
        "selected": selected_keys.contains(&"updated_at".to_string()),
    }));

    let title_field = def.title_field();

    for f in &def.fields {
        if Some(f.name.as_str()) == title_field {
            continue;
        }

        if is_column_eligible(&f.field_type) {
            options.push(json!({
                "key": f.name,
                "label": field_label(f),
                "selected": selected_keys.contains(&f.name),
            }));
        }
    }

    options
}

/// Build filter field metadata for the filter builder UI.
///
/// `_status` is exposed as a filterable field for collections with
/// drafts, with `published` / `draft` as the value choices. To see
/// "all", the user removes the `_status` filter row entirely (or
/// clicks Clear all) — drafts surface to the top of the unfiltered
/// list via `apply_order_by`'s `_status ASC` prepend, so there's no
/// separate "All" affordance to maintain. The list handler reads
/// `?where[_status][equals]=X` via the dedicated
/// `extract_status_filter` path because system columns (`_*`) are
/// off-limits to the generic user-filter pipeline.
pub(super) fn build_filter_fields(def: &CollectionDefinition) -> Vec<Value> {
    let mut fields = Vec::new();

    if def.has_drafts() {
        fields.push(json!({
            "key": "_status",
            "label": "status",
            "field_type": "select",
            "options": [
                { "label": "published", "value": "published" },
                { "label": "draft", "value": "draft" },
            ],
        }));
    }

    fields.push(json!({
        "key": "created_at",
        "label": "created",
        "field_type": "date",
    }));

    fields.push(json!({
        "key": "updated_at",
        "label": "updated",
        "field_type": "date",
    }));

    for f in &def.fields {
        if !is_column_eligible(&f.field_type) {
            continue;
        }
        let mut field_info = json!({
            "key": f.name,
            "label": field_label(f),
            "field_type": f.field_type.as_str(),
        });

        if !f.options.is_empty() {
            let opts: Vec<Value> = f
                .options
                .iter()
                .map(|o| {
                    json!({
                        "label": o.label.resolve_default(),
                        "value": o.value,
                    })
                })
                .collect();

            field_info["options"] = json!(opts);
        }

        fields.push(field_info);
    }

    fields
}

/// Build active filter pills from parsed filter clauses.
pub(super) fn build_filter_pills(
    parsed: &[FilterClause],
    def: &CollectionDefinition,
    raw_query: &str,
) -> Vec<Value> {
    parsed
        .iter()
        .filter_map(|clause| {
            let FilterClause::Single(filter) = clause else {
                return None;
            };

            let field_label_str = match filter.field.as_str() {
                "created_at" => "created".to_string(),
                "updated_at" => "updated".to_string(),
                "_status" => "status".to_string(),
                name => def
                    .fields
                    .iter()
                    .find(|f| f.name == name)
                    .map_or_else(|| auto_label_from_name(name), field_label),
            };

            let (op_label, value) = match &filter.op {
                FilterOp::Equals(v) => ("is", v.clone()),
                FilterOp::NotEquals(v) => ("is not", v.clone()),
                FilterOp::Contains(v) => ("contains", v.clone()),
                FilterOp::Like(v) => ("like", v.clone()),
                FilterOp::GreaterThan(v) => (">", v.clone()),
                FilterOp::LessThan(v) => ("<", v.clone()),
                FilterOp::GreaterThanOrEqual(v) => (">=", v.clone()),
                FilterOp::LessThanOrEqual(v) => ("<=", v.clone()),
                FilterOp::Exists => ("exists", String::new()),
                FilterOp::NotExists => ("not exists", String::new()),
                _ => return None,
            };

            let filter_key = format!("where[{}][{}]", filter.field, op_to_param_name(&filter.op));

            let remove_query: Vec<&str> = raw_query
                .split('&')
                .filter(|p| {
                    let decoded = url_decode(p.split('=').next().unwrap_or(""));
                    decoded != filter_key
                })
                .collect();

            let remove_url = if remove_query.is_empty() {
                String::new()
            } else {
                format!("?{}", remove_query.join("&"))
            };

            Some(json!({
                "field_label": field_label_str,
                "op": op_label,
                "value": value,
                "remove_url": remove_url,
            }))
        })
        .collect()
}

/// Map a `FilterOp` to its URL parameter name — the canonical
/// [`FilterOp::op_name`] for every op except `In`/`NotIn`, which the admin URL
/// grammar reconstructs as repeated `equals`/`not_equals` entries.
pub(super) fn op_to_param_name(op: &FilterOp) -> &'static str {
    match op {
        FilterOp::In(_) => "equals",
        FilterOp::NotIn(_) => "not_equals",
        other => other.op_name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::VersionsConfig;
    use crate::core::{
        FieldAdmin, FieldDefinition, FieldType, LocalizedString, SelectOption, collection::*,
        document::DocumentBuilder,
    };

    fn test_collection() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Select)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Draft".into()), "draft"),
                    SelectOption::new(LocalizedString::Plain("Published".into()), "published"),
                ])
                .build(),
            FieldDefinition::builder("body", FieldType::Richtext).build(),
            FieldDefinition::builder("views", FieldType::Number).build(),
            FieldDefinition::builder("active", FieldType::Checkbox).build(),
            FieldDefinition::builder("date", FieldType::Date).build(),
        ];
        def.admin = AdminConfig {
            use_as_title: Some("title".to_string()),
            ..Default::default()
        };
        def
    }

    #[test]
    fn field_label_uses_admin_label() {
        let f = FieldDefinition::builder("my_field", FieldType::Text)
            .admin(
                FieldAdmin::builder()
                    .label(LocalizedString::Plain("Custom Label".into()))
                    .build(),
            )
            .build();
        assert_eq!(field_label(&f), "Custom Label");
    }

    #[test]
    fn field_label_falls_back_to_name() {
        let f = FieldDefinition::builder("my_field", FieldType::Text).build();
        assert_eq!(field_label(&f), "My Field");
    }

    fn test_url_ctx(sort: Option<&str>) -> ListUrlContext<'_> {
        ListUrlContext {
            base_url: "/admin/collections/posts",
            search: None,
            sort,
            where_params: "",
        }
    }

    #[test]
    fn resolve_columns_defaults() {
        let def = test_collection();
        let cols = resolve_columns(&def, None, &test_url_ctx(None));
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "created_at");
    }

    #[test]
    fn resolve_columns_user_cols() {
        let def = test_collection();
        let user_cols = vec!["status".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None));
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0]["key"], "status");
        assert_eq!(cols[1]["key"], "views");
    }

    /// Regression: a has-many relationship is a
    /// valid list column but is NOT sortable (no parent column) — its
    /// `sortable` flag must match `validate_sort`, or the rendered sort
    /// header 400s on click. A sortable scalar field stays sortable.
    #[test]
    fn has_many_relationship_column_is_not_sortable() {
        use crate::core::field::RelationshipConfig;

        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tag", true)) // has_many
                .build(),
        );

        let user_cols = vec!["tags".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None));

        let tags = cols
            .iter()
            .find(|c| c["key"] == "tags")
            .expect("tags column");
        assert_eq!(
            tags["sortable"],
            serde_json::json!(false),
            "a has-many relationship column must not be sortable"
        );
        let views = cols
            .iter()
            .find(|c| c["key"] == "views")
            .expect("views column");
        assert_eq!(
            views["sortable"],
            serde_json::json!(true),
            "a scalar number column stays sortable"
        );
    }

    #[test]
    fn resolve_columns_filters_invalid() {
        let def = test_collection();
        let user_cols = vec!["title".to_string(), "body".to_string(), "views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None));
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "views");
    }

    /// With no per-user selection, the collection's configured
    /// `admin.list_columns` is used as the default (in order), overriding the
    /// built-in `created_at`-only fallback.
    #[test]
    fn resolve_columns_uses_collection_list_columns_default() {
        let mut def = test_collection();
        def.admin.list_columns = vec!["status".into(), "views".into(), "created_at".into()];

        let cols = resolve_columns(&def, None, &test_url_ctx(None));
        let keys: Vec<&str> = cols.iter().map(|c| c["key"].as_str().unwrap()).collect();
        assert_eq!(keys, vec!["status", "views", "created_at"]);
    }

    /// A per-user column selection wins over the collection's default.
    #[test]
    fn resolve_columns_user_selection_overrides_collection_default() {
        let mut def = test_collection();
        def.admin.list_columns = vec!["status".into()];
        let user_cols = vec!["views".to_string()];

        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(None));
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0]["key"], "views");
    }

    #[test]
    fn resolve_columns_sort_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(Some("views")));
        assert_eq!(cols[0]["is_sorted_asc"], true);
        assert_eq!(cols[0]["is_sorted_desc"], false);
    }

    #[test]
    fn resolve_columns_sort_desc_state() {
        let def = test_collection();
        let user_cols = vec!["views".to_string()];
        let cols = resolve_columns(&def, Some(&user_cols), &test_url_ctx(Some("-views")));
        assert_eq!(cols[0]["is_sorted_asc"], false);
        assert_eq!(cols[0]["is_sorted_desc"], true);
    }

    #[test]
    fn compute_cells_status_badge() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("_status".into(), json!("draft"));

        let columns = vec![json!({"key": "_status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_badge"], true);
        assert_eq!(cells[0]["value"], "draft");
    }

    #[test]
    fn compute_cells_select_shows_label() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("status".into(), json!("published"));

        let columns = vec![json!({"key": "status"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["value"], "Published");
    }

    #[test]
    fn compute_cells_checkbox() {
        let def = test_collection();
        let mut doc = DocumentBuilder::new("1").build();
        doc.fields.insert("active".into(), json!(1));

        let columns = vec![json!({"key": "active"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_bool"], true);
        assert_eq!(cells[0]["value"], true);
    }

    #[test]
    fn compute_cells_date() {
        let def = test_collection();
        let doc = DocumentBuilder::new("1")
            .created_at(Some("2024-01-15"))
            .build();

        let columns = vec![json!({"key": "created_at"})];
        let cells = compute_cells(&doc, &columns, &def);
        assert_eq!(cells[0]["is_date"], true);
    }

    #[test]
    fn build_column_options_includes_eligible() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()]);
        let keys: Vec<&str> = opts.iter().filter_map(|o| o["key"].as_str()).collect();
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"updated_at"));
        assert!(keys.contains(&"status")); // select - eligible
        assert!(keys.contains(&"views")); // number - eligible
        assert!(!keys.contains(&"body")); // richtext - ineligible
        assert!(!keys.contains(&"title")); // title field - excluded
    }

    #[test]
    fn build_column_options_marks_selected() {
        let def = test_collection();
        let opts = build_column_options(&def, &["status".to_string()]);
        let status_opt = opts.iter().find(|o| o["key"] == "status").unwrap();
        assert_eq!(status_opt["selected"], true);
        let views_opt = opts.iter().find(|o| o["key"] == "views").unwrap();
        assert_eq!(views_opt["selected"], false);
    }

    #[test]
    fn build_filter_fields_includes_eligible() {
        let def = test_collection();
        let fields = build_filter_fields(&def);
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"status"));
        assert!(keys.contains(&"views"));
        assert!(!keys.contains(&"body")); // richtext ineligible
    }

    #[test]
    fn build_filter_fields_select_has_options() {
        let def = test_collection();
        let fields = build_filter_fields(&def);
        let status_field = fields.iter().find(|f| f["key"] == "status").unwrap();
        let opts = status_field["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "draft");
    }

    #[test]
    fn build_filter_fields_omits_status_without_drafts() {
        let def = test_collection();
        let fields = build_filter_fields(&def);
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
        assert!(!keys.contains(&"_status"));
    }

    #[test]
    fn build_filter_fields_includes_status_with_drafts() {
        let mut def = test_collection();
        def.versions = Some(VersionsConfig::new(true, 10));

        let fields = build_filter_fields(&def);
        let status_field = fields.iter().find(|f| f["key"] == "_status").unwrap();
        let opts = status_field["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "published");
        assert_eq!(opts[1]["value"], "draft");
    }

    #[test]
    fn op_to_param_name_all_ops() {
        assert_eq!(op_to_param_name(&FilterOp::Equals("x".into())), "equals");
        assert_eq!(
            op_to_param_name(&FilterOp::NotEquals("x".into())),
            "not_equals"
        );
        assert_eq!(
            op_to_param_name(&FilterOp::Contains("x".into())),
            "contains"
        );
        assert_eq!(
            op_to_param_name(&FilterOp::GreaterThan("x".into())),
            "greater_than"
        );
        assert_eq!(
            op_to_param_name(&FilterOp::LessThan("x".into())),
            "less_than"
        );
        assert_eq!(
            op_to_param_name(&FilterOp::GreaterThanOrEqual("x".into())),
            "greater_than_or_equal"
        );
        assert_eq!(
            op_to_param_name(&FilterOp::LessThanOrEqual("x".into())),
            "less_than_or_equal"
        );
        assert_eq!(op_to_param_name(&FilterOp::Exists), "exists");
        assert_eq!(op_to_param_name(&FilterOp::NotExists), "not_exists");
    }
}
