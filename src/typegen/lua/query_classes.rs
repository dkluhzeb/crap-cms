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

/// The virtual relevance sort a `search` term enables. Always best-first, so
/// it has no descending form.
const RANK_SORT: &str = "_rank";

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

/// The `where` keys: every queryable column but the system ones (`_status`,
/// `_deleted_at`), which a user filter may not name — the `trash` / `draft`
/// flags reach that data.
fn where_keys(columns: &[String]) -> impl Iterator<Item = &String> {
    columns.iter().filter(|c| !is_system_filter_path(c))
}

/// The `order_by` values: each sortable queryable column ascending and
/// descending — a has-many list holds no order and is left out — plus the
/// search relevance sort.
fn order_by_values(col: &CollectionDefinition, columns: &[String]) -> Vec<String> {
    columns
        .iter()
        .filter(|c| is_valid_sort_column(c, col))
        .flat_map(|c| [format!("\"{c}\""), format!("\"-{c}\"")])
        .chain([format!("\"{RANK_SORT}\"")])
        .collect()
}

/// `crap.where.*` and `crap.query.*` for `col`.
pub(super) fn render_query_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    // crap.where.* — typed filter keys. `(exact)` makes the class
    // closed so an unknown column name in a `where` table (e.g.
    // `where = { not_a_column = "x" }`) is flagged instead of being
    // silently accepted via Lua's default open-class shape.
    let columns = queryable_columns(col);
    w!(out, "---@class (exact) crap.where.{pascal}");
    for key in where_keys(&columns) {
        w!(out, "---@field {}? crap.FilterValue", lua_field_key(key));
    }
    out.push('\n');

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
    use crate::core::{FieldDefinition, FieldType, VersionsConfig};

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
}
