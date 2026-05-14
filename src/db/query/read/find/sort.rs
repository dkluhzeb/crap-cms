//! Sort resolution and ORDER BY clause emission for [`super::find`].
//!
//! Drafts surface to the top of admin-style listings: when the
//! collection has drafts and the user's sort isn't already `_status`,
//! [`apply_order_by`] prepends `_status ASC` so `'draft'` rows come
//! before `'published'` regardless of the configured `default_sort`
//! (e.g. `-published_at`, where drafts have a NULL key and would
//! otherwise sort last).

use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::core::{CollectionDefinition, FieldDefinition, FieldType};
use crate::db::query::cursor::SortDirection;
use crate::db::query::filter::resolve_filter_column;
use crate::db::query::helpers::prefixed_name;
use crate::db::query::{self, resolve_sort as resolve_order};
use crate::db::{FindQuery, LocaleContext};

/// Resolve sort column, direction, and cursor mode from query.
pub(super) fn resolve_sort(
    def: &CollectionDefinition,
    query: &FindQuery,
) -> Result<(String, SortDirection, bool)> {
    let has_cursor = query.after_cursor.is_some() || query.before_cursor.is_some();

    if has_cursor && query.offset.is_some() {
        bail!("Cannot use both cursor and offset — they are mutually exclusive");
    }

    if query.after_cursor.is_some() && query.before_cursor.is_some() {
        bail!("Cannot use both after_cursor and before_cursor — they are mutually exclusive");
    }

    let (sort_col, sort_dir) = resolve_order(query.order_by.as_deref(), def.timestamps);

    if !is_valid_sort_column(&sort_col, def) {
        bail!(
            "Invalid sort column '{}' — not a column on '{}'",
            sort_col,
            def.slug
        );
    }

    Ok((sort_col, sort_dir, query.before_cursor.is_some()))
}

/// Append ORDER BY clause with stable tiebreaker.
///
/// The effective ORDER BY for a drafts-enabled collection is
/// `(_status ASC, sort_col DIR, id DIR)`. Both `_status` and the
/// inner sort flip when `using_before` is set so `before_cursor`
/// walks the same composite order in reverse. Cursor keysets must
/// encode `_status` to remain symmetric — see
/// [`super::cursor::apply_cursor_keyset`] and
/// [`crate::db::query::cursor::CursorData::status_val`]. When the
/// WHERE clause already pins `_status` to a single value (filter to
/// drafts/published only, or `include_drafts=false` injection) the
/// prepended `_status ASC` is a no-op and the user's sort is
/// preserved exactly.
pub(super) fn apply_order_by(
    sort_col: &str,
    sort_dir: SortDirection,
    using_before: bool,
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
    sql: &mut String,
) -> Result<()> {
    let effective_dir = if using_before {
        sort_dir.flip()
    } else {
        sort_dir
    };
    let resolved = resolve_filter_column(sort_col, def, locale_ctx)?;

    let prepend_status = query::cursor::cursor_status_active(def.has_drafts(), sort_col);
    let status_dir = if using_before {
        SortDirection::Desc
    } else {
        SortDirection::Asc
    };
    let status_prefix = if prepend_status {
        format!("_status {status_dir}, ")
    } else {
        String::new()
    };

    if sort_col == "id" {
        let _ = write!(sql, " ORDER BY {status_prefix}id {effective_dir}");
    } else {
        let _ = write!(
            sql,
            " ORDER BY {status_prefix}{resolved} {effective_dir}, id {effective_dir}"
        );
    }

    Ok(())
}

/// Recurse through field definitions looking for a column name.
/// Layout wrappers (Row, Collapsible, Tabs) promote their children to
/// parent-level columns, so we recurse into them. Group sub-fields use
/// `group__subfield` naming for DB columns.
fn check_fields(col: &str, fields: &[FieldDefinition], prefix: &str) -> bool {
    fields.iter().any(|f| {
        let full_name = prefixed_name(prefix, &f.name);

        if full_name == col && f.has_parent_column() {
            return true;
        }

        match f.field_type {
            FieldType::Group => check_fields(col, &f.fields, &full_name),
            FieldType::Row | FieldType::Collapsible => check_fields(col, &f.fields, prefix),
            FieldType::Tabs => f
                .tabs
                .iter()
                .any(|tab| check_fields(col, &tab.fields, prefix)),
            _ => false,
        }
    })
}

/// Check whether a sort column name corresponds to a real column on the collection table.
pub(super) fn is_valid_sort_column(col: &str, def: &CollectionDefinition) -> bool {
    // System columns that always exist
    if matches!(
        col,
        "id" | "created_at" | "updated_at" | "_status" | "_deleted_at" | "_ref_count"
    ) {
        return true;
    }

    // User-defined fields that have a parent column (has-one scalar fields).
    check_fields(col, &def.fields, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CollectionDefinition;
    use crate::core::field::*;
    use crate::db::FindQuery;
    use crate::db::query::read::find::find;
    use crate::db::query::read::find::test_helpers::*;

    #[test]
    fn invalid_sort_column_returns_error_not_500() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .order_by(Some("nonexistent_column".to_string()))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err(), "Should reject invalid sort column");
        // Caught by validate_query_fields before reaching SQL
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid field"),
            "Should be a validation error, got: {err_msg}"
        );
    }

    #[test]
    fn sort_column_inside_row_is_valid() {
        let mut def = CollectionDefinition::new("events");
        def.fields = vec![FieldDefinition {
            name: "date_row".to_string(),
            field_type: FieldType::Row,
            fields: vec![FieldDefinition {
                name: "start_date".to_string(),
                field_type: FieldType::Date,
                ..Default::default()
            }],
            ..Default::default()
        }];

        assert!(
            is_valid_sort_column("start_date", &def),
            "Field inside Row should be valid sort column"
        );
    }

    #[test]
    fn sort_column_inside_collapsible_is_valid() {
        let mut def = CollectionDefinition::new("items");
        def.fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "priority".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            }],
            ..Default::default()
        }];

        assert!(
            is_valid_sort_column("priority", &def),
            "Field inside Collapsible should be valid sort column"
        );
    }

    #[test]
    fn sort_column_inside_tabs_is_valid() {
        let mut def = CollectionDefinition::new("pages");
        def.fields = vec![
            FieldDefinition::builder("content_tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Main",
                    vec![FieldDefinition::builder("title", FieldType::Text).build()],
                )])
                .build(),
        ];

        assert!(
            is_valid_sort_column("title", &def),
            "Field inside Tabs should be valid sort column"
        );
    }

    #[test]
    fn sort_column_nonexistent_is_invalid() {
        let def = test_def();
        assert!(
            !is_valid_sort_column("nonexistent", &def),
            "Nonexistent field should be invalid sort column"
        );
    }

    #[test]
    fn sort_column_group_sub_field_is_valid() {
        let mut def = CollectionDefinition::new("pages");
        def.fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        assert!(
            is_valid_sort_column("seo__title", &def),
            "Group sub-field should be valid sort column with __ prefix"
        );
        assert!(
            !is_valid_sort_column("title", &def),
            "Bare sub-field name should not be valid without group prefix"
        );
    }

    #[test]
    fn sort_column_group_in_tabs_is_valid() {
        let mut def = CollectionDefinition::new("pages");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "SEO",
                    vec![
                        FieldDefinition::builder("seo", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("title", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];

        assert!(
            is_valid_sort_column("seo__title", &def),
            "Group sub-field inside Tabs should be valid sort column"
        );
    }
}
