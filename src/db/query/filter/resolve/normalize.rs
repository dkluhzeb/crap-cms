//! Rewrite dot-notation Group paths to flat `__`-joined column names.

use crate::core::{FieldDefinition, FieldType, find_field};
use crate::db::FilterClause;

use super::container::container_root;

/// Rewrite dot notation for group fields: `seo.meta_title` → `seo__meta_title`.
///
/// Array, Blocks, and Relationship fields keep their dots (resolved at SQL
/// generation time via subqueries). Only a path naming a group's value is
/// converted here, because it maps to a flat `{group}__{sub}` column on the
/// parent table; a path reaching an array, blocks or has-many field inside a
/// group (`seo.items.name`) is kept as written — the resolver finds that
/// field's own join table, and errors name the path the caller wrote.
pub fn normalize_filter_fields(filters: &mut [FilterClause], fields: &[FieldDefinition]) {
    for clause in filters.iter_mut() {
        normalize_clause(clause, fields);
    }
}

/// Rewrite the dotted group form of an `order_by` (`seo.title`, `-seo.title`)
/// to the flat column it sorts by (`seo__title`, `-seo__title`) — the form
/// filters accept, so a sort and a filter name a group's value alike.
/// Anything else is left as written, for the sort validation to judge.
#[must_use]
pub fn normalize_order_by(order_by: &str, fields: &[FieldDefinition]) -> String {
    let (descending, column) = match order_by.strip_prefix('-') {
        Some(column) => ("-", column),
        None => ("", order_by),
    };

    let mut column = column.to_string();

    normalize_field_name(&mut column, fields);

    format!("{descending}{column}")
}

/// Rewrite group dot-paths in one [`FilterClause`] tree node, recursing through
/// `And`/`Or`.
fn normalize_clause(clause: &mut FilterClause, fields: &[FieldDefinition]) {
    match clause {
        FilterClause::Single(f) => normalize_field_name(&mut f.field, fields),
        FilterClause::And(subs) | FilterClause::Or(subs) => {
            for c in subs.iter_mut() {
                normalize_clause(c, fields);
            }
        }
    }
}

fn normalize_field_name(field: &mut String, fields: &[FieldDefinition]) {
    if !field.contains('.') || container_root(field, fields).is_some() {
        return;
    }

    let Some(first_segment) = field.split('.').next() else {
        return;
    };

    if is_group_field(first_segment, fields) {
        *field = field.replace('.', "__");
    }
}

/// Check if a field name refers to a Group, recursing into transparent layout wrappers.
fn is_group_field(name: &str, fields: &[FieldDefinition]) -> bool {
    find_field(name, fields).is_some_and(|f| f.field_type == FieldType::Group)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldTab, FieldType};
    use crate::db::query::{Filter, FilterClause, FilterOp};

    fn make_field(name: &str, ft: FieldType, localized: bool) -> FieldDefinition {
        FieldDefinition::builder(name, ft)
            .localized(localized)
            .build()
    }

    fn make_array_field(name: &str, sub_fields: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Array)
            .fields(sub_fields)
            .build()
    }

    fn make_blocks_field(name: &str, blocks: Vec<crate::core::BlockDefinition>) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Blocks)
            .blocks(blocks)
            .build()
    }

    #[test]
    fn normalize_group_dot_to_double_underscore() {
        let fields = vec![make_field("seo", FieldType::Group, false)];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.meta_title".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__meta_title"),
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn normalize_preserves_array_dots() {
        let fields = vec![make_array_field(
            "items",
            vec![make_field("name", FieldType::Text, false)],
        )];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "items.name".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "items.name"),
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn normalize_preserves_blocks_dots() {
        let fields = vec![make_blocks_field("content", vec![])];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "content.body".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "content.body"),
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn normalize_in_or_groups() {
        let fields = vec![make_field("seo", FieldType::Group, false)];
        let mut filters = vec![FilterClause::or_groups(vec![
            vec![Filter {
                field: "seo.title".into(),
                op: FilterOp::Equals("a".into()),
            }],
            vec![Filter {
                field: "seo.desc".into(),
                op: FilterOp::Equals("b".into()),
            }],
        ])];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Or(alts) => {
                let (FilterClause::Single(f0), FilterClause::Single(f1)) = (&alts[0], &alts[1])
                else {
                    panic!("expected single-filter alternatives");
                };
                assert_eq!(f0.field, "seo__title");
                assert_eq!(f1.field, "seo__desc");
            }
            other => panic!("Expected Or, got {other:?}"),
        }
    }

    #[test]
    fn normalize_no_dots_passthrough() {
        let fields = vec![make_field("title", FieldType::Text, false)];
        let mut filters = vec![FilterClause::Single(Filter {
            field: "title".into(),
            op: FilterOp::Equals("test".into()),
        })];
        normalize_filter_fields(&mut filters, &fields);
        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "title"),
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn normalize_group_inside_row() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build();
        let row = FieldDefinition::builder("layout", FieldType::Row)
            .fields(vec![group])
            .build();
        let fields = vec![row];

        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.title".to_string(),
            op: FilterOp::Equals("test".to_string()),
        })];
        normalize_filter_fields(&mut filters, &fields);

        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__title"),
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn normalize_group_inside_tabs() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build();
        let tabs = FieldDefinition::builder("layout", FieldType::Tabs)
            .tabs(vec![FieldTab {
                label: "Main".to_string(),
                description: None,
                fields: vec![group],
            }])
            .build();
        let fields = vec![tabs];

        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.title".to_string(),
            op: FilterOp::Equals("test".to_string()),
        })];
        normalize_filter_fields(&mut filters, &fields);

        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__title"),
            _ => panic!("expected single"),
        }
    }

    #[test]
    fn normalize_group_inside_collapsible() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build();
        let collapsible = FieldDefinition::builder("advanced", FieldType::Collapsible)
            .fields(vec![group])
            .build();
        let fields = vec![collapsible];

        let mut filters = vec![FilterClause::Single(Filter {
            field: "seo.title".to_string(),
            op: FilterOp::Equals("test".to_string()),
        })];
        normalize_filter_fields(&mut filters, &fields);

        match &filters[0] {
            FilterClause::Single(f) => assert_eq!(f.field, "seo__title"),
            _ => panic!("expected single"),
        }
    }

    /// Regression: a path reaching an array, blocks or has-many field inside a
    /// group was flattened whole (`seo__links__url`), which named no column
    /// and no join table. It is kept as written for the resolver.
    #[test]
    fn normalize_keeps_a_path_into_a_groups_join_field() {
        let seo = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                make_array_field("links", vec![make_field("url", FieldType::Text, false)]),
                make_field("title", FieldType::Text, false),
            ])
            .build();
        let fields = vec![seo];

        for (path, expected) in [
            ("seo.links.url", "seo.links.url"),
            ("seo__links.url", "seo__links.url"),
            ("seo.title", "seo__title"),
        ] {
            let mut filters = vec![FilterClause::Single(Filter {
                field: path.to_string(),
                op: FilterOp::Equals("x".to_string()),
            })];

            normalize_filter_fields(&mut filters, &fields);

            let FilterClause::Single(f) = &filters[0] else {
                panic!("expected single");
            };
            assert_eq!(f.field, expected, "{path}");
        }
    }

    /// Regression: `order_by` accepted only the flat `seo__title` while
    /// filters took `seo.title` too. Both spellings sort, in both directions.
    #[test]
    fn normalize_order_by_flattens_a_dotted_group_value() {
        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![make_field("title", FieldType::Text, false)])
                .build(),
            make_field("title", FieldType::Text, false),
        ];

        assert_eq!(normalize_order_by("seo.title", &fields), "seo__title");
        assert_eq!(normalize_order_by("-seo.title", &fields), "-seo__title");
        assert_eq!(normalize_order_by("-seo__title", &fields), "-seo__title");
        assert_eq!(normalize_order_by("title", &fields), "title");
        assert_eq!(normalize_order_by("nope.x", &fields), "nope.x");
    }
}
