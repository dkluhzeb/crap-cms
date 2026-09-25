//! List filters: the filter builder's field metadata and the active-filter
//! pills.

use std::slice::from_ref;

use serde_json::{Value, json};

use super::access::ListFieldAccess;
use crate::{
    admin::{
        Translations,
        handlers::shared::{
            StatusFilter, auto_label_from_name, field_label, is_column_eligible, url_decode,
        },
    },
    core::collection::CollectionDefinition,
    db::query::{Filter, FilterClause, FilterOp},
};

/// Query parameters that position the list inside one result set. A link that
/// changes the filters must drop them: the old page or cursor points into a
/// result set that no longer exists.
const POSITION_PARAMS: [&str; 3] = ["page", "after_cursor", "before_cursor"];

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
///
/// A field is offered only when the `where[]` parser accepts it (it has a
/// column on the collection's own table — a has-many relationship does not)
/// and the viewer is offered it at all (see [`ListFieldAccess`]).
pub(in crate::admin::handlers::collections) fn build_filter_fields(
    def: &CollectionDefinition,
    access: &ListFieldAccess,
) -> Vec<Value> {
    let mut fields = Vec::new();

    if def.has_drafts() {
        fields.push(json!({
            "key": "_status",
            "label": "status",
            // Its own type: the server accepts only `equals` on `_status`,
            // so the builder must not offer the select operators.
            "field_type": "status",
            "options": [
                { "label": "published", "value": "published" },
                { "label": "draft", "value": "draft" },
            ],
        }));
    }

    if def.timestamps {
        fields.push(json!({ "key": "created_at", "label": "created", "field_type": "date" }));
        fields.push(json!({ "key": "updated_at", "label": "updated", "field_type": "date" }));
    }

    let filterable = def
        .fields
        .iter()
        .filter(|f| is_column_eligible(&f.field_type) && f.has_parent_column() && access.offers(f));

    for f in filterable {
        let mut field_info = json!({
            "key": f.name,
            "label": field_label(f),
            "field_type": f.field_type.as_str(),
        });

        if !f.options.is_empty() {
            let opts: Vec<Value> = f
                .options
                .iter()
                .map(|o| json!({ "label": o.label.resolve_current(), "value": o.value }))
                .collect();

            field_info["options"] = json!(opts);
        }

        fields.push(field_info);
    }

    fields
}

/// Everything the active-filter pills are built from. Constructed once per
/// list request.
pub(in crate::admin::handlers::collections) struct FilterPillInputs<'a> {
    /// The parsed user filters (`where[…]`).
    pub parsed: &'a [FilterClause],
    pub def: &'a CollectionDefinition,
    /// The request's raw query string — the source every remove link edits.
    pub raw_query: &'a str,
    /// The list path the remove links point at.
    pub base_url: &'a str,
    /// The typed `_status` filter, when one is active.
    pub status_filter: Option<&'a StatusFilter>,
    pub translations: &'a Translations,
    /// The viewer's UI locale, for the operator and status labels.
    pub locale: &'a str,
}

impl FilterPillInputs<'_> {
    fn t(&self, key: &str) -> String {
        self.translations.get(self.locale, key).to_string()
    }

    /// The list URL with every entry whose decoded key and value `drop`
    /// matches removed, and with the position params reset.
    fn remove_url(&self, drop: impl Fn(&str, &str) -> bool) -> String {
        let kept: Vec<&str> = self
            .raw_query
            .split('&')
            .filter(|p| !p.is_empty())
            .filter(|p| {
                let (key, value) = p.split_once('=').unwrap_or((p, ""));
                let key = url_decode(key);

                !POSITION_PARAMS.contains(&key.as_str()) && !drop(&key, &url_decode(value))
            })
            .collect();

        if kept.is_empty() {
            return self.base_url.to_string();
        }

        format!("{}?{}", self.base_url, kept.join("&"))
    }
}

/// The label a filter pill shows for a field key.
fn pill_field_label(def: &CollectionDefinition, field: &str) -> String {
    match field {
        "created_at" => "created".to_string(),
        "updated_at" => "updated".to_string(),
        "_status" => "status".to_string(),
        name => def
            .fields
            .iter()
            .find(|f| f.name == name)
            .map_or_else(|| auto_label_from_name(name), field_label),
    }
}

/// The translation key of an operator's pill label, and the value it shows.
fn pill_op(op: &FilterOp) -> (&'static str, String) {
    match op {
        FilterOp::Equals(v) => ("op_is", v.clone()),
        FilterOp::NotEquals(v) => ("op_is_not", v.clone()),
        FilterOp::Contains(v) => ("op_contains", v.clone()),
        FilterOp::Like(v) => ("op_like", v.clone()),
        FilterOp::GreaterThan(v) => ("op_gt", v.clone()),
        FilterOp::LessThan(v) => ("op_lt", v.clone()),
        FilterOp::GreaterThanOrEqual(v) => ("op_gte", v.clone()),
        FilterOp::LessThanOrEqual(v) => ("op_lte", v.clone()),
        FilterOp::In(vals) => ("op_is_any_of", vals.join(", ")),
        FilterOp::NotIn(vals) => ("op_is_none_of", vals.join(", ")),
        FilterOp::Exists => ("op_exists", String::new()),
        FilterOp::NotExists => ("op_not_exists", String::new()),
    }
}

/// The value a URL row spells for `op`, or `None` for an operator that takes
/// none (`exists`/`not_exists`, spelled with an empty value or `true`).
fn url_operand(op: &FilterOp) -> Option<&str> {
    match op {
        FilterOp::Equals(v)
        | FilterOp::NotEquals(v)
        | FilterOp::Like(v)
        | FilterOp::Contains(v)
        | FilterOp::GreaterThan(v)
        | FilterOp::LessThan(v)
        | FilterOp::GreaterThanOrEqual(v)
        | FilterOp::LessThanOrEqual(v) => Some(v),
        FilterOp::In(_) | FilterOp::NotIn(_) | FilterOp::Exists | FilterOp::NotExists => None,
    }
}

/// The pill for one top-level filter. Its remove link drops that one row —
/// matched by key and value, so a second row on the same field and operator
/// (two AND-ed `equals`, say) keeps its own pill.
fn filter_pill(inputs: &FilterPillInputs<'_>, filter: &Filter) -> Value {
    let (op_key, value) = pill_op(&filter.op);
    let filter_key = format!("where[{}][{}]", filter.field, filter.op.op_name());
    let operand = url_operand(&filter.op);

    json!({
        "field_label": pill_field_label(inputs.def, &filter.field),
        "op": inputs.t(op_key),
        "value": value,
        "remove_url": inputs.remove_url(|key, value| {
            key == filter_key && operand.is_none_or(|operand| operand == value)
        }),
    })
}

/// Whether a decoded query key is a `_status` filter entry, top-level or
/// inside an OR group.
fn is_status_key(key: &str) -> bool {
    key == "where[_status][equals]"
        || (key.starts_with("where[or][") && key.ends_with("][_status][equals]"))
}

/// The pill for the typed `_status` filter, which never reaches the parsed
/// filter list.
fn status_pill(inputs: &FilterPillInputs<'_>, values: &[String]) -> Value {
    let op_key = if values.len() > 1 {
        "op_is_any_of"
    } else {
        "op_is"
    };
    let value = values
        .iter()
        .map(|v| inputs.t(v))
        .collect::<Vec<_>>()
        .join(", ");

    json!({
        "field_label": "status",
        "op": inputs.t(op_key),
        "value": value,
        "remove_url": inputs.remove_url(|key, _| is_status_key(key)),
    })
}

/// Build active filter pills: one per top-level filter, plus one for the
/// `_status` filter. OR groups get no pill — [`active_filter_count`] still
/// counts them.
pub(in crate::admin::handlers::collections) fn build_filter_pills(
    inputs: &FilterPillInputs<'_>,
) -> Vec<Value> {
    let mut pills: Vec<Value> = inputs
        .parsed
        .iter()
        .filter_map(|clause| match clause {
            FilterClause::Single(filter) => Some(filter_pill(inputs, filter)),
            _ => None,
        })
        .collect();

    if let Some(status) = inputs.status_filter {
        pills.extend(status_pills(inputs, status));
    }

    pills
}

/// The pills of the `_status` filter: one naming the statuses it admits — or,
/// when its rows contradict each other and admit none, one per row as written,
/// the way any other field's AND-ed rows each get a pill.
fn status_pills(inputs: &FilterPillInputs<'_>, status: &StatusFilter) -> Vec<Value> {
    if !status.matches_nothing() {
        return vec![status_pill(inputs, status.admitted())];
    }

    status
        .named()
        .iter()
        .map(|value| status_pill(inputs, from_ref(value)))
        .collect()
}

/// How many filters are active, for the toolbar badge: every pill plus every
/// OR group (which has no pill of its own).
pub(in crate::admin::handlers::collections) fn active_filter_count(
    pills: &[Value],
    parsed: &[FilterClause],
) -> usize {
    let or_groups = parsed
        .iter()
        .filter(|c| !matches!(c, FilterClause::Single(_)))
        .count();

    pills.len() + or_groups
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::Path};

    use super::*;
    use crate::{
        admin::handlers::{
            collections::list_helpers::test_helpers::test_collection, shared::extract_status_filter,
        },
        core::{FieldDefinition, FieldType, RelationshipConfig, VersionsConfig},
    };

    fn translations() -> Translations {
        Translations::load(Path::new("/nonexistent"))
    }

    fn pills_for(
        parsed: &[FilterClause],
        raw_query: &str,
        status_filter: Option<&StatusFilter>,
        locale: &str,
    ) -> Vec<Value> {
        let def = test_collection();
        let translations = translations();

        build_filter_pills(&FilterPillInputs {
            parsed,
            def: &def,
            raw_query,
            base_url: "/admin/collections/posts",
            status_filter,
            translations: &translations,
            locale,
        })
    }

    fn single(field: &str, op: FilterOp) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        })
    }

    #[test]
    fn build_filter_fields_includes_eligible() {
        let def = test_collection();
        let fields = build_filter_fields(&def, &ListFieldAccess::default());
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
        assert!(keys.contains(&"created_at"));
        assert!(keys.contains(&"status"));
        assert!(keys.contains(&"views"));
        assert!(!keys.contains(&"body")); // richtext ineligible
    }

    /// Regression: a collection defined with `timestamps = false` has no
    /// timestamp columns to filter on.
    #[test]
    fn build_filter_fields_timestamps_need_timestamps() {
        let mut def = test_collection();
        def.timestamps = false;

        let fields = build_filter_fields(&def, &ListFieldAccess::default());
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();

        assert!(!keys.contains(&"created_at"));
        assert!(!keys.contains(&"updated_at"));
        assert!(keys.contains(&"views"));
    }

    #[test]
    fn build_filter_fields_select_has_options() {
        let def = test_collection();
        let fields = build_filter_fields(&def, &ListFieldAccess::default());
        let status_field = fields.iter().find(|f| f["key"] == "status").unwrap();
        let opts = status_field["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "draft");
    }

    #[test]
    fn build_filter_fields_omits_status_without_drafts() {
        let def = test_collection();
        let fields = build_filter_fields(&def, &ListFieldAccess::default());
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
        assert!(!keys.contains(&"_status"));
    }

    #[test]
    fn build_filter_fields_includes_status_with_drafts() {
        let mut def = test_collection();
        def.versions = Some(VersionsConfig::new(true, 10));

        let fields = build_filter_fields(&def, &ListFieldAccess::default());
        let status_field = fields.iter().find(|f| f["key"] == "_status").unwrap();
        assert_eq!(
            status_field["field_type"], "status",
            "`_status` gets its own equals-only operator list, not the select one"
        );
        let opts = status_field["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "published");
        assert_eq!(opts[1]["value"], "draft");
    }

    /// Regression: hidden and read-denied fields were offered as filters (a
    /// click answered 403 for the whole collection), and a has-many
    /// relationship was offered although the `where[]` parser rejects it.
    #[test]
    fn build_filter_fields_skips_fields_the_query_would_refuse() {
        let mut def = test_collection();
        def.fields.push(
            FieldDefinition::builder("internal", FieldType::Text)
                .hidden(true)
                .build(),
        );
        def.fields.push(
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tag", true))
                .build(),
        );
        let access = ListFieldAccess::new(HashSet::from(["views".to_string()]));

        let fields = build_filter_fields(&def, &access);
        let keys: Vec<&str> = fields.iter().filter_map(|f| f["key"].as_str()).collect();
        assert!(!keys.contains(&"internal"));
        assert!(!keys.contains(&"tags"));
        assert!(!keys.contains(&"views"));
        assert!(keys.contains(&"status"));
    }

    /// Regression: pill operators were hard-coded English and removing a
    /// filter kept the page/cursor of the old result set.
    #[test]
    fn filter_pill_is_translated_and_resets_the_position() {
        let parsed = [single("views", FilterOp::GreaterThan("5".into()))];
        let raw = "page=3&after_cursor=abc&where[views][greater_than]=5&sort=title";

        let pills = pills_for(&parsed, raw, None, "de");
        assert_eq!(pills.len(), 1);
        assert_eq!(pills[0]["op"], ">");
        assert_eq!(
            pills[0]["remove_url"],
            "/admin/collections/posts?sort=title"
        );

        let parsed = [single("title", FilterOp::Equals("x".into()))];
        let pills = pills_for(&parsed, "where[title][equals]=x", None, "de");
        assert_eq!(pills[0]["op"], "ist");
    }

    /// Removing the only filter leads back to the bare list path. An empty
    /// `href` used to re-request the current, still filtered, URL.
    #[test]
    fn removing_the_last_filter_links_to_the_bare_list() {
        let parsed = [single("title", FilterOp::Equals("x".into()))];
        let pills = pills_for(&parsed, "where%5Btitle%5D%5Bequals%5D=x", None, "en");

        assert_eq!(pills[0]["remove_url"], "/admin/collections/posts");
    }

    /// Each AND-ed row and the `_status` filter get a pill; OR groups are
    /// counted in the badge although they have no pill. Removing one of two
    /// rows on the same field and operator keeps the other.
    #[test]
    fn rows_status_and_or_filters_are_counted() {
        let parsed = [
            single("title", FilterOp::Equals("a".into())),
            single("title", FilterOp::Equals("b".into())),
            FilterClause::or_groups(vec![
                vec![Filter {
                    field: "views".into(),
                    op: FilterOp::Equals("1".into()),
                }],
                vec![Filter {
                    field: "views".into(),
                    op: FilterOp::Equals("2".into()),
                }],
            ]),
        ];
        let raw = "where[title][equals]=a&where[title][equals]=b\
                   &where[or][0][0][_status][equals]=draft&where[or][0][1][_status][equals]=published";
        let status = extract_status_filter(raw);

        let pills = pills_for(&parsed, raw, status.as_ref(), "en");
        assert_eq!(pills.len(), 3);
        assert_eq!(pills[0]["value"], "a");
        assert_eq!(pills[0]["op"], "is");
        assert_eq!(
            pills[1]["remove_url"],
            "/admin/collections/posts?where[title][equals]=a\
             &where[or][0][0][_status][equals]=draft&where[or][0][1][_status][equals]=published"
        );
        assert_eq!(pills[2]["field_label"], "status");
        assert_eq!(pills[2]["value"], "Draft, Published");
        assert_eq!(
            pills[2]["remove_url"],
            "/admin/collections/posts?where[title][equals]=a&where[title][equals]=b"
        );

        assert_eq!(active_filter_count(&pills, &parsed), 4);
    }

    /// Contradicting `_status` rows admit no status: each row keeps a pill of
    /// its own instead of reading as "any of".
    #[test]
    fn contradicting_status_rows_get_a_pill_each() {
        let raw = "where[_status][equals]=draft&where[_status][equals]=published";
        let status = extract_status_filter(raw);

        let pills = pills_for(&[], raw, status.as_ref(), "en");

        assert_eq!(pills.len(), 2);
        assert_eq!(pills[0]["value"], "Draft");
        assert_eq!(pills[0]["op"], "is");
        assert_eq!(pills[1]["value"], "Published");
    }
}
