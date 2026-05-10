//! Rewrite dot-notation Group paths to flat `__`-joined column names.

use crate::core::{FieldDefinition, FieldType};
use crate::db::FilterClause;

/// Rewrite dot notation for group fields: `seo.meta_title` → `seo__meta_title`.
///
/// Array, Blocks, and Relationship fields keep their dots (resolved at SQL
/// generation time via subqueries). Only Group fields are converted here
/// because they map to flat `{group}__{sub}` columns on the parent table.
pub fn normalize_filter_fields(filters: &mut [FilterClause], fields: &[FieldDefinition]) {
    for clause in filters.iter_mut() {
        match clause {
            FilterClause::Single(f) => normalize_field_name(&mut f.field, fields),
            FilterClause::Or(groups) => {
                for group in groups.iter_mut() {
                    for f in group.iter_mut() {
                        normalize_field_name(&mut f.field, fields);
                    }
                }
            }
        }
    }
}

fn normalize_field_name(field: &mut String, fields: &[FieldDefinition]) {
    if !field.contains('.') {
        return;
    }
    let first_segment = match field.split('.').next() {
        Some(s) => s,
        None => return,
    };

    if is_group_field(first_segment, fields) {
        *field = field.replace('.', "__");
    }
}

/// Check if a field name refers to a Group, recursing into transparent layout wrappers.
fn is_group_field(name: &str, fields: &[FieldDefinition]) -> bool {
    for f in fields {
        if f.name == name && f.field_type == FieldType::Group {
            return true;
        }

        // Recurse into transparent layout wrappers
        match f.field_type {
            FieldType::Row | FieldType::Collapsible if is_group_field(name, &f.fields) => {
                return true;
            }
            FieldType::Tabs => {
                for tab in &f.tabs {
                    if is_group_field(name, &tab.fields) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }

    false
}

#[cfg(test)]
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
            other => panic!("Expected Single, got {:?}", other),
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
            other => panic!("Expected Single, got {:?}", other),
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
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn normalize_in_or_groups() {
        let fields = vec![make_field("seo", FieldType::Group, false)];
        let mut filters = vec![FilterClause::Or(vec![
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
            FilterClause::Or(groups) => {
                assert_eq!(groups[0][0].field, "seo__title");
                assert_eq!(groups[1][0].field, "seo__desc");
            }
            other => panic!("Expected Or, got {:?}", other),
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
            other => panic!("Expected Single, got {:?}", other),
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
}
