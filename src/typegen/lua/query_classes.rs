//! The typed filter and query classes of a collection: `crap.where.*` and
//! `crap.query.*`.

use crate::{
    core::CollectionDefinition,
    db::query::get_column_names,
    service::is_hidden_query_path,
    typegen::{helpers::w, idents::lua_field_key},
};

/// The columns a find, count or search may filter and sort on: every column
/// except those of a `hidden` field, which no query may reference (the same
/// predicate the read path rejects them with).
fn queryable_columns(col: &CollectionDefinition) -> Vec<String> {
    get_column_names(col)
        .into_iter()
        .filter(|c| !is_hidden_query_path(col, c))
        .collect()
}

/// `crap.where.*` and `crap.query.*` for `col`.
pub(super) fn render_query_classes(out: &mut String, col: &CollectionDefinition, pascal: &str) {
    // crap.where.* — typed filter keys. `(exact)` makes the class
    // closed so an unknown column name in a `where` table (e.g.
    // `where = { not_a_column = "x" }`) is flagged instead of being
    // silently accepted via Lua's default open-class shape. The
    // columns are the queryable DB schema — anything outside the
    // list is a typo or a column no query may reference.
    let columns = queryable_columns(col);
    w!(out, "---@class (exact) crap.where.{pascal}");
    for col_name in &columns {
        w!(
            out,
            "---@field {}? crap.FilterValue",
            lua_field_key(col_name)
        );
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
    let order_by_union: Vec<String> = columns
        .iter()
        .flat_map(|c| [format!("\"{c}\""), format!("\"-{c}\"")])
        .collect();
    w!(
        out,
        "---@class (exact) crap.query.{pascal} : crap.FindQuery"
    );
    w!(out, "---@field where? crap.where.{pascal}");
    w!(out, "---@field order_by? {}", order_by_union.join("|"));
    w!(out, "---@field limit? integer");
    w!(out, "---@field offset? integer");
    w!(out, "---@field locale? string");
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldType};

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
