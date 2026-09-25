//! The typed filter and query classes of a collection: `crap.where.*` and
//! `crap.query.*`. Both are derived from the predicates the read path
//! validates a query with, so a key the types offer is one the runtime
//! accepts.

use crate::{
    core::CollectionDefinition,
    db::query::{get_column_names, get_valid_filter_columns, read::is_valid_sort_column},
    service::{is_hidden_query_path, is_system_filter_path},
    typegen::{helpers::w, idents::lua_field_key},
};

use super::filter_paths::{dotted_group_column, row_filter_paths};

/// The virtual relevance sort a `search` term enables. Always best-first, so
/// it has no descending form.
const RANK_SORT: &str = "_rank";

/// The `where` key holding OR groups.
const OR_KEY: &str = "or";

/// The columns a find, count or search may name: the runtime's filter
/// columns, in column order, except those of a `hidden` field, which no query
/// may reference (the same predicate the read path rejects them with).
fn queryable_columns(col: &CollectionDefinition) -> Vec<String> {
    let valid = get_valid_filter_columns(col, None);

    get_column_names(col)
        .into_iter()
        .filter(|c| valid.contains(c) && !is_hidden_query_path(col, c))
        .collect()
}

/// A column and, for a group's value, its dotted spelling after it
/// (`seo__title`, `seo.title`) — both name the same column everywhere.
fn with_dotted_spelling(col: &CollectionDefinition, column: &str) -> Vec<String> {
    let mut spellings = vec![column.to_string()];
    spellings.extend(dotted_group_column(col, column));

    spellings
}

/// The `where` keys: every queryable column but the system ones (`_status`,
/// `_deleted_at`), which a user filter may not name — the `trash` / `draft`
/// flags reach that data — each group value in both spellings, then every
/// path into the rows of an array, blocks or has-many reference field, a
/// `hidden` field's excluded.
fn where_keys(col: &CollectionDefinition, columns: &[String]) -> Vec<String> {
    let paths = columns
        .iter()
        .filter(|c| !is_system_filter_path(c))
        .flat_map(|c| with_dotted_spelling(col, c));

    let row_paths = row_filter_paths(&col.fields)
        .into_iter()
        .filter(|p| !is_hidden_query_path(col, p));

    paths.chain(row_paths).collect()
}

/// The `order_by` values: each sortable queryable column ascending and
/// descending — a group's value in both spellings; a has-many list holds no
/// order and is left out — plus the search relevance sort.
fn order_by_values(col: &CollectionDefinition, columns: &[String]) -> Vec<String> {
    columns
        .iter()
        .filter(|c| is_valid_sort_column(c, col))
        .flat_map(|c| with_dotted_spelling(col, c))
        .flat_map(|c| [format!("\"{c}\""), format!("\"-{c}\"")])
        .chain([format!("\"{RANK_SORT}\"")])
        .collect()
}

/// `crap.where_group.*` (one AND-group of conditions) and `crap.where.*` (a
/// group plus its `or` alternatives). `(exact)` makes both closed so an
/// unknown path in a `where` table (e.g. `where = { not_a_column = "x" }`) is
/// flagged instead of being silently accepted via Lua's default open-class
/// shape. An `or` group holds conditions only — no nested `or`.
fn render_where_classes(
    out: &mut String,
    col: &CollectionDefinition,
    pascal: &str,
    columns: &[String],
) {
    w!(out, "---@class (exact) crap.where_group.{pascal}");
    for key in where_keys(col, columns) {
        w!(out, "---@field {}? crap.FilterValue", lua_field_key(&key));
    }
    out.push('\n');

    w!(
        out,
        "---@class (exact) crap.where.{pascal} : crap.where_group.{pascal}"
    );
    w!(
        out,
        "---@field {}? crap.where_group.{pascal}[] Alternatives: a document matches when any one group matches",
        lua_field_key(OR_KEY)
    );
    out.push('\n');
}

/// `crap.where.*` and `crap.query.*` for `col`.
pub(super) fn render_query_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    let columns = queryable_columns(col);
    render_where_classes(out, col, pascal, &columns);

    // crap.query.* — typed query options. Extends `crap.FindQuery` so
    // a `---@type crap.query.X` variable can be passed to the
    // `query?: crap.FindQuery` overload slot on `find` (LuaLS doesn't
    // duck-type class params, so inheritance is mandatory for the
    // assignment to type-check).
    //
    // Note on `(exact)`: even with `Lua.type.checkTableShape = true`,
    // LuaLS doesn't enforce exact-class validation on inline table
    // literals when the exact class is the parent of a non-exact one
    // or is reached via inheritance (LuaLS issue #2288). The
    // narrowing on `where` / `order_by` still drives autocomplete.
    // For strict where-key validation, users have to extract the
    // where table into a `---@type crap.where.X` local first.
    w!(
        out,
        "---@class (exact) crap.query.{pascal} : crap.FindQuery"
    );
    w!(out, "---@field where? crap.where.{pascal}");
    w!(
        out,
        "---@field order_by? {}",
        order_by_values(col, &columns).join("|")
    );
    w!(out, "---@field limit? integer");
    w!(out, "---@field offset? integer");
    w!(out, "---@field locale? string");
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig, VersionsConfig},
        db::{
            Filter, FilterClause, FilterOp, FindQuery, InMemoryConn,
            query::{
                filter::{build_where_clause, normalize_filter_fields},
                validate_query_fields,
            },
        },
    };

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn array(name: &str, fields: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Array)
            .fields(fields)
            .build()
    }

    fn relationship(name: &str, target: &str, has_many: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new(target, has_many))
            .build()
    }

    /// Every path kind the query grammar has: localized, timezone and
    /// has-many scalar columns, a layout wrapper, a group holding a value, an
    /// array and a has-many reference, an array whose rows hold a group, a
    /// nested array, nested blocks and a hidden value, a blocks field, and
    /// has-one, has-many and polymorphic references.
    fn query_grammar_fixture() -> CollectionDefinition {
        let mut related = RelationshipConfig::new("users", true);
        related.polymorphic = vec!["users".into(), "tags".into()];

        let mut col = CollectionDefinition::new("pages");
        col.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("published_at", FieldType::Date)
                .timezone(true)
                .build(),
            FieldDefinition::builder("keywords", FieldType::Text)
                .has_many(true)
                .build(),
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![text("subtitle")])
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    text("title"),
                    array("links", vec![text("url")]),
                    relationship("tags", "tags", true),
                ])
                .build(),
            array(
                "items",
                vec![
                    text("label"),
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![text("key")])
                        .build(),
                    array("notes", vec![text("body")]),
                    FieldDefinition::builder("parts", FieldType::Blocks)
                        .blocks(vec![BlockDefinition::new("bolt", vec![text("size")])])
                        .build(),
                    FieldDefinition::builder("secret", FieldType::Text)
                        .hidden(true)
                        .build(),
                ],
            ),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![
                    BlockDefinition::new("hero", vec![text("heading")]),
                    BlockDefinition::new("quote", vec![text("heading"), text("cite")]),
                ])
                .build(),
            relationship("tags", "tags", true),
            relationship("author", "users", false),
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(related)
                .build(),
        ];

        col
    }

    /// Every `where` key the types declare passes the read path's validation
    /// and builds a filter, and every `order_by` value passes validation —
    /// the types never offer a path the runtime refuses.
    #[test]
    fn every_typed_query_path_is_accepted_by_the_runtime() {
        let col = query_grammar_fixture();
        let columns = queryable_columns(&col);
        let conn = InMemoryConn::open();

        for key in where_keys(&col, &columns) {
            let mut filters = vec![FilterClause::Single(Filter {
                field: key.clone(),
                op: FilterOp::Equals("x".to_string()),
            })];
            normalize_filter_fields(&mut filters, &col.fields);

            let query = FindQuery {
                filters,
                ..FindQuery::default()
            };

            validate_query_fields(&col, &query, None)
                .unwrap_or_else(|e| panic!("where key {key}: {e:#}"));
            build_where_clause(
                &conn,
                &query.filters,
                "pages",
                &col.fields,
                None,
                &mut Vec::new(),
            )
            .unwrap_or_else(|e| panic!("where key {key}: {e:#}"));
        }

        for value in order_by_values(&col, &columns) {
            let order = value.trim_matches('"').to_string();
            let query = FindQuery {
                order_by: Some(order.clone()),
                search: Some("term".to_string()),
                ..FindQuery::default()
            };

            validate_query_fields(&col, &query, None)
                .unwrap_or_else(|e| panic!("order_by {order}: {e:#}"));
        }
    }

    /// Regression: the typed `where` keys and `order_by` values declared a
    /// `hidden` field's columns, which every find, count and search rejects.
    #[test]
    fn hidden_columns_are_not_queryable() {
        let mut col = CollectionDefinition::new("posts");
        col.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("token", FieldType::Text)
                        .hidden(true)
                        .build(),
                    FieldDefinition::builder("summary", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut out = String::new();
        render_query_classes(&mut out, &col, "Posts");

        assert!(out.contains("---@field title? crap.FilterValue"), "{out}");
        assert!(
            out.contains("---@field seo__summary? crap.FilterValue"),
            "{out}"
        );
        assert!(out.contains(r#""-title""#), "{out}");
        assert!(!out.contains("secret"), "{out}");
        assert!(!out.contains("seo__token"), "{out}");
    }

    /// Regression: the typed `order_by` offered a has-many list column (the
    /// runtime refuses to sort by one) and lacked the documented `_rank`;
    /// `where` offered `_status` / `_deleted_at`, which a user filter may not
    /// name. `order_by` keeps the system columns the runtime sorts by.
    #[test]
    fn query_keys_follow_the_runtime_predicates() {
        let mut col = CollectionDefinition::new("posts");
        col.soft_delete = true;
        col.versions = Some(VersionsConfig::new(true, 0));
        col.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
        ];

        let mut out = String::new();
        render_query_classes(&mut out, &col, "Posts");

        let where_class = &out[..out.find("crap.query.Posts").expect("query class")];
        assert!(
            where_class.contains("---@field tags? crap.FilterValue"),
            "{out}"
        );
        assert!(!where_class.contains("_status"), "{out}");
        assert!(!where_class.contains("_deleted_at"), "{out}");

        let order_by = out
            .lines()
            .find(|l| l.starts_with("---@field order_by?"))
            .expect("order_by line");
        assert!(order_by.contains(r#""-title""#), "{order_by}");
        assert!(order_by.contains(r#""_status""#), "{order_by}");
        assert!(order_by.contains(r#""_rank""#), "{order_by}");
        assert!(!order_by.contains(r#""-_rank""#), "{order_by}");
        assert!(!order_by.contains("tags"), "{order_by}");
    }

    /// Regression: a leading-digit column was declared as a bare `where` key
    /// (`---@field 2fa? crap.FilterValue`), which `LuaLS` misparses. It is a
    /// quoted key; `order_by` names it as a plain string literal.
    #[test]
    fn non_identifier_columns_are_quoted_where_keys() {
        let mut col = CollectionDefinition::new("accounts");
        col.fields = vec![FieldDefinition::builder("2fa", FieldType::Text).build()];

        let mut out = String::new();
        render_query_classes(&mut out, &col, "Accounts");

        assert!(
            out.contains("---@field [\"2fa\"]? crap.FilterValue"),
            "{out}"
        );
        assert!(out.contains(r#""-2fa""#), "{out}");
    }

    /// Regression: the closed `crap.where.*` class declared only the flat
    /// columns, so the `or` key, a group value's dotted spelling and every
    /// path into array/blocks/has-many rows — all accepted by the runtime —
    /// were undefined-field diagnostics, and `order_by` lacked the dotted
    /// group spelling.
    #[test]
    fn where_and_order_by_follow_the_whole_path_grammar() {
        let mut col = CollectionDefinition::new("pages");
        col.fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    FieldDefinition::builder("secret", FieldType::Text)
                        .hidden(true)
                        .build(),
                ])
                .build(),
        ];

        let mut out = String::new();
        render_query_classes(&mut out, &col, "Pages");

        let group = &out[..out
            .find("---@class (exact) crap.where.Pages")
            .expect("where")];
        for line in [
            "---@class (exact) crap.where_group.Pages",
            "---@field seo__title? crap.FilterValue",
            "---@field [\"seo.title\"]? crap.FilterValue",
            "---@field [\"items.id\"]? crap.FilterValue",
            "---@field [\"items.label\"]? crap.FilterValue",
        ] {
            assert!(group.contains(line), "{line}: {out}");
        }
        assert!(!group.contains("items.secret"), "{out}");
        assert!(
            !group.contains("[\"or\"]"),
            "an or group nests no or: {out}"
        );

        assert!(
            out.contains(
                "---@class (exact) crap.where.Pages : crap.where_group.Pages\n\
                 ---@field [\"or\"]? crap.where_group.Pages[]"
            ),
            "{out}"
        );

        let order_by = out
            .lines()
            .find(|l| l.starts_with("---@field order_by?"))
            .expect("order_by line");
        for value in ["\"seo__title\"", "\"-seo.title\"", "\"seo.title\""] {
            assert!(order_by.contains(value), "{value}: {order_by}");
        }
        assert!(!order_by.contains("items"), "{order_by}");
    }
}
