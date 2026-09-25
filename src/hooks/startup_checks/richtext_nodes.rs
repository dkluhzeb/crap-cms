//! Every custom node a rich text field lists in `admin.nodes` must be
//! registered with `crap.richtext.register_node`.
//!
//! A node name is only a string on the field, and registration may happen in
//! any file `init.lua` loads, so this runs once every definition has loaded.
//! Without it an unregistered name (a registration file that is never
//! `require`d, a typo) booted: the toolbar lacked the node, its attrs went
//! unvalidated, and stored content using it could not be opened in the editor.

use anyhow::{Result, bail};

use crate::{
    core::{FieldDefinition, FieldType, Registry, walk_all_fields},
    hooks::startup_checks::hook_refs::field_source_label,
};

/// Reject every rich text field — at any depth, in any collection or global —
/// whose `admin.nodes` names a node that is not registered.
///
/// # Errors
///
/// Returns an aggregated error naming each offending field and node.
pub fn validate_richtext_nodes(registry: &Registry) -> Result<()> {
    let mut offenders = Vec::new();

    let definitions = registry
        .collections
        .iter()
        .map(|(slug, def)| (format!("collection '{slug}'"), &def.fields))
        .chain(
            registry
                .globals
                .iter()
                .map(|(slug, def)| (format!("global '{slug}'"), &def.fields)),
        );

    for (label, fields) in definitions {
        check_fields(registry, fields, &label, &mut offenders);
    }

    if offenders.is_empty() {
        return Ok(());
    }

    // Registry maps iterate in arbitrary order; sort for a stable message.
    offenders.sort();

    bail!(
        "Rich text field lists a custom node that is not registered \
         (register it with crap.richtext.register_node in init.lua or a file it requires):\n  - {}",
        offenders.join("\n  - ")
    )
}

fn check_fields(
    registry: &Registry,
    fields: &[FieldDefinition],
    label: &str,
    out: &mut Vec<String>,
) {
    walk_all_fields(fields, &mut Vec::new(), &mut |field, path| {
        if field.field_type != FieldType::Richtext {
            return;
        }

        for node in &field.admin.nodes {
            if registry.get_richtext_node(node).is_none() {
                out.push(format!(
                    "{}: node '{node}' is not registered",
                    field_source_label(label, path, field)
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CollectionDefinition, FieldAdmin, GlobalDefinition, RichtextNodeDef};

    fn richtext_with_nodes(nodes: &[&str]) -> FieldDefinition {
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .nodes(nodes.iter().map(ToString::to_string).collect())
                    .build(),
            )
            .build()
    }

    fn registry_with_cta() -> Registry {
        let mut registry = Registry::new();
        registry.register_richtext_node(RichtextNodeDef::builder("cta", "CTA").build());
        registry
    }

    #[test]
    fn registered_nodes_pass() {
        let mut registry = registry_with_cta();
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![richtext_with_nodes(&["cta"])];
        registry.register_collection(def);

        assert!(validate_richtext_nodes(&registry).is_ok());
    }

    /// Regression: an unregistered node name was silently dropped — at the
    /// top level and nested alike.
    #[test]
    fn unregistered_nodes_are_reported_at_any_depth() {
        let mut registry = registry_with_cta();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![richtext_with_nodes(&["cta", "banner"])])
                .build(),
        ];
        registry.register_collection(def);

        let mut global = GlobalDefinition::new("site");
        global.fields = vec![richtext_with_nodes(&["ghost"])];
        registry.register_global(global);

        let msg = format!("{:#}", validate_richtext_nodes(&registry).unwrap_err());

        assert!(
            msg.contains("collection 'posts' field 'items' field 'body': node 'banner'"),
            "{msg}"
        );
        assert!(
            msg.contains("global 'site' field 'body': node 'ghost'"),
            "{msg}"
        );
        assert!(!msg.contains("'cta'"), "{msg}");
    }
}
