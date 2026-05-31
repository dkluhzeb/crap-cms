//! Recursive walker that dispatches each field to the per-type sync helper.
//!
//! Split into two halves: a pure planner ([`plan_join_tables`]) that walks
//! the field tree and produces the flat list of join tables to sync — with
//! all the prefix/localized-inheritance/layout-transparency logic — and a
//! thin executor that runs the DDL for each plan entry. The walk carries no
//! I/O, so it is unit-tested directly.

use anyhow::Result;

use crate::config::LocaleConfig;
use crate::core::{FieldDefinition, FieldType};
use crate::db::DbConnection;
use crate::db::query::helpers::prefixed_name;

use super::array::sync_array_table;
use super::blocks::sync_blocks_table;
use super::relationship::sync_relationship_table;

/// Which per-type sync helper a planned join table dispatches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinTableKind {
    /// Has-many relationship / upload (`sync_relationship_table`).
    Relationship,
    /// Array field (`sync_array_table`).
    Array,
    /// Blocks field (`sync_blocks_table`).
    Blocks,
}

/// A single join table that needs to exist for a collection — the pure
/// output of walking the field tree, before any DDL runs.
struct JoinTablePlan<'a> {
    kind: JoinTableKind,
    /// The field that owns the join table (carries relationship config,
    /// sub-fields, etc.). Unused by the blocks branch but kept uniform.
    field: &'a FieldDefinition,
    /// `__`-joined path from the collection root: group prefixes applied,
    /// layout wrappers (Row/Collapsible/Tabs) transparent.
    full_name: String,
    /// Whether the join table needs a `_locale` column.
    has_locale_col: bool,
}

/// Walk the field tree and produce the flat, ordered list of join tables to
/// sync. Pure — no DB I/O. Depth-first, matching the executor's apply order.
fn plan_join_tables<'a>(
    fields: &'a [FieldDefinition],
    locale_config: &LocaleConfig,
) -> Vec<JoinTablePlan<'a>> {
    let mut plans = Vec::new();
    collect_join_table_plans(fields, locale_config, "", false, &mut plans);
    plans
}

/// Recursive core of [`plan_join_tables`]. `prefix` accumulates the `__`
/// group path; `inherited_localized` propagates a localized group down to its
/// children (mirroring the `_locale`-column decision).
fn collect_join_table_plans<'a>(
    fields: &'a [FieldDefinition],
    locale_config: &LocaleConfig,
    prefix: &str,
    inherited_localized: bool,
    plans: &mut Vec<JoinTablePlan<'a>>,
) {
    for field in fields {
        let has_locale_col = (inherited_localized || field.localized) && locale_config.is_enabled();
        let full_name = prefixed_name(prefix, &field.name);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                plans.push(JoinTablePlan {
                    kind: JoinTableKind::Relationship,
                    field,
                    full_name,
                    has_locale_col,
                });
            }
            FieldType::Array => {
                plans.push(JoinTablePlan {
                    kind: JoinTableKind::Array,
                    field,
                    full_name,
                    has_locale_col,
                });
            }
            FieldType::Blocks => {
                plans.push(JoinTablePlan {
                    kind: JoinTableKind::Blocks,
                    field,
                    full_name,
                    has_locale_col,
                });
            }
            FieldType::Group => {
                collect_join_table_plans(
                    &field.fields,
                    locale_config,
                    &full_name,
                    inherited_localized || field.localized,
                    plans,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_join_table_plans(
                    &field.fields,
                    locale_config,
                    prefix,
                    inherited_localized,
                    plans,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_join_table_plans(
                        &tab.fields,
                        locale_config,
                        prefix,
                        inherited_localized,
                        plans,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Run the DDL for a single planned join table.
fn execute_join_table_plan(
    conn: &dyn DbConnection,
    collection_slug: &str,
    plan: &JoinTablePlan<'_>,
    locale_config: &LocaleConfig,
) -> Result<()> {
    match plan.kind {
        JoinTableKind::Relationship => sync_relationship_table(
            conn,
            collection_slug,
            plan.field,
            &plan.full_name,
            plan.has_locale_col,
            locale_config,
        ),
        JoinTableKind::Array => sync_array_table(
            conn,
            collection_slug,
            plan.field,
            &plan.full_name,
            plan.has_locale_col,
            locale_config,
        ),
        JoinTableKind::Blocks => sync_blocks_table(
            conn,
            collection_slug,
            &plan.full_name,
            plan.has_locale_col,
            locale_config,
        ),
    }
}

/// Sync join tables for has-many relationships and array fields.
pub(in crate::db::migrate) fn sync_join_tables(
    conn: &dyn DbConnection,
    collection_slug: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    for plan in plan_join_tables(fields, locale_config) {
        execute_join_table_plan(conn, collection_slug, &plan, locale_config)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldTab, RelationshipConfig};

    use super::*;

    fn rel(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build()
    }

    /// Flatten a plan into `(kind, full_name, has_locale_col)` for assertions.
    fn plan_tuples(
        fields: &[FieldDefinition],
        cfg: &LocaleConfig,
    ) -> Vec<(JoinTableKind, String, bool)> {
        plan_join_tables(fields, cfg)
            .into_iter()
            .map(|p| (p.kind, p.full_name, p.has_locale_col))
            .collect()
    }

    fn enabled_locales() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn plans_each_join_bearing_field_type() {
        let fields = vec![
            rel("tags"),
            FieldDefinition::builder("items", FieldType::Array).build(),
            FieldDefinition::builder("content", FieldType::Blocks).build(),
        ];
        assert_eq!(
            plan_tuples(&fields, &LocaleConfig::default()),
            vec![
                (JoinTableKind::Relationship, "tags".into(), false),
                (JoinTableKind::Array, "items".into(), false),
                (JoinTableKind::Blocks, "content".into(), false),
            ]
        );
    }

    #[test]
    fn group_prefixes_children_with_double_underscore() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![rel("tags")])
                .build(),
        ];
        assert_eq!(
            plan_tuples(&fields, &LocaleConfig::default()),
            vec![(JoinTableKind::Relationship, "meta__tags".into(), false)]
        );
    }

    #[test]
    fn layout_wrappers_are_transparent_no_prefix() {
        let fields = vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("items", FieldType::Array).build(),
                ])
                .build(),
        ];
        assert_eq!(
            plan_tuples(&fields, &LocaleConfig::default()),
            vec![(JoinTableKind::Array, "items".into(), false)]
        );
    }

    #[test]
    fn tabs_recurse_into_each_tab() {
        let fields = vec![
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new("A", vec![rel("a_tags")]),
                    FieldTab::new("B", vec![rel("b_tags")]),
                ])
                .build(),
        ];
        assert_eq!(
            plan_tuples(&fields, &LocaleConfig::default()),
            vec![
                (JoinTableKind::Relationship, "a_tags".into(), false),
                (JoinTableKind::Relationship, "b_tags".into(), false),
            ]
        );
    }

    #[test]
    fn localized_field_gets_locale_column_only_when_locales_enabled() {
        let localized_rel = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .localized(true)
            .build();

        // Disabled config → no locale column even though the field is localized.
        assert_eq!(
            plan_tuples(
                std::slice::from_ref(&localized_rel),
                &LocaleConfig::default()
            ),
            vec![(JoinTableKind::Relationship, "tags".into(), false)]
        );

        // Enabled config → locale column.
        assert_eq!(
            plan_tuples(&[localized_rel], &enabled_locales()),
            vec![(JoinTableKind::Relationship, "tags".into(), true)]
        );
    }

    #[test]
    fn localized_group_propagates_locale_column_to_children() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .localized(true)
                .fields(vec![rel("tags")]) // child not itself localized
                .build(),
        ];
        // Inherited localization → child join table still gets a locale column.
        assert_eq!(
            plan_tuples(&fields, &enabled_locales()),
            vec![(JoinTableKind::Relationship, "meta__tags".into(), true)]
        );
    }
}
