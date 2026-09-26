//! Display conditions are refused on fields inside array/blocks rows.
//!
//! A display condition (`admin.condition`) judges the form's data, and the
//! form has one data scope: the document. A row is a repeated scope of its
//! own — each row would need its own data, its own evaluation per render, per
//! new row and per live re-evaluation — so a condition on a field inside a
//! row could only ever be ignored. Refusing it at load says so instead of
//! leaving the field always shown.

use anyhow::{Result, bail};

use crate::core::{FieldDefinition, FieldType, Registry, SchemaStep, walk_all_fields};

use super::hook_refs::field_source_label;

/// Whether `path` descends through an array or blocks field.
fn inside_row(path: &[SchemaStep<'_>]) -> bool {
    path.iter().any(|step| {
        matches!(
            step,
            SchemaStep::Field(field)
                if matches!(field.field_type, FieldType::Array | FieldType::Blocks)
        )
    })
}

/// Record every field of `fields` that sits inside a row and declares a
/// display condition.
fn collect_row_conditions(fields: &[FieldDefinition], source: &str, out: &mut Vec<String>) {
    walk_all_fields(fields, &mut Vec::new(), &mut |field, path| {
        if field.admin.condition.is_some() && inside_row(path) {
            out.push(field_source_label(source, path, field));
        }
    });
}

/// Refuse `admin.condition` on any field inside an array or blocks row.
///
/// # Errors
///
/// Returns an aggregated error naming every such field.
pub fn validate_row_conditions(registry: &Registry) -> Result<()> {
    let mut found: Vec<String> = Vec::new();

    for (slug, def) in &registry.collections {
        collect_row_conditions(&def.fields, &format!("collection '{slug}'"), &mut found);
    }

    for (slug, def) in &registry.globals {
        collect_row_conditions(&def.fields, &format!("global '{slug}'"), &mut found);
    }

    if found.is_empty() {
        return Ok(());
    }

    found.sort();

    bail!(
        "admin.condition on a field inside an array or blocks row:\n  - {}\n\n\
         A display condition judges the whole form; a row has no scope of its own, \
         so the condition could never apply. Move the field out of the row, or \
         remove its admin.condition.",
        found.join("\n  - ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{BlockDefinition, CollectionDefinition, HookRef};

    fn conditioned(name: &str) -> FieldDefinition {
        let mut field = FieldDefinition::builder(name, FieldType::Text).build();
        field.admin.condition = Some(HookRef::new("hooks.conditions.show"));

        field
    }

    fn registry_with(fields: Vec<FieldDefinition>) -> Registry {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields;

        let mut registry = Registry::new();
        registry.register_collection(def);

        registry
    }

    /// Regression: a condition inside a row was accepted and silently never
    /// evaluated, so the field it was meant to hide always showed.
    #[test]
    fn a_condition_inside_an_array_or_blocks_row_is_refused() {
        let registry = registry_with(vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![conditioned("caption")])
                .build(),
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "quote",
                    vec![
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![conditioned("source")])
                            .build(),
                    ],
                )])
                .build(),
        ]);

        let msg = validate_row_conditions(&registry).unwrap_err().to_string();

        assert!(msg.contains("'caption'"), "{msg}");
        assert!(msg.contains("'source'"), "{msg}");
    }

    #[test]
    fn a_condition_in_a_group_or_layout_wrapper_is_accepted() {
        let registry = registry_with(vec![
            conditioned("title"),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![conditioned("desc")])
                .build(),
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![conditioned("side")])
                .build(),
        ]);

        assert!(validate_row_conditions(&registry).is_ok());
    }
}
