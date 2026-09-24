//! What a completed ref-count backfill counted: the computation version, the
//! configured locales and the schema's reference topology, folded into the
//! gate value each collection and global is stamped with.

use crate::{
    config::LocaleConfig,
    core::{FieldChildren, FieldDefinition, Registry, field_children},
    db::migrate::helpers::versioned_fingerprint,
};

/// Current backfill computation version. Stored as the meta *value* (not
/// baked into the key, which stays stable). Bump this whenever the ref-count
/// computation changes so existing databases recompute once on the next
/// startup. v2 added recursion into relationships nested in groups/arrays/
/// blocks and has-many relationships stored inside blocks.
pub(super) const BACKFILL_VERSION: &str = "2";

/// The gate a completed backfill stores: the computation version, the locale
/// configuration it counted under, and the reference topology it counted.
///
/// The walk visits exactly the configured locales' columns, so the locale list
/// is part of the result. Gating on the version alone left a removed locale's
/// references counted forever — phantom counts that block deletes — and never
/// counted an added locale's at all.
///
/// The same holds for the reference-bearing fields themselves: removing a
/// relationship or upload field, retargeting it, turning it into a list or
/// localizing it changes what the walk counts. The topology is taken over the
/// WHOLE registry and stored in every slug's gate, because the count on one
/// collection is made of every other collection's references — including a
/// collection removed from the schema, whose references must stop counting.
pub(super) fn gate_value(registry: &Registry, locale_config: &LocaleConfig) -> String {
    let mut parts = vec![locale_config.fingerprint()];
    parts.extend(reference_topology(registry));

    versioned_fingerprint(BACKFILL_VERSION, &parts)
}

/// One entry per collection and global that holds references — sorted, so
/// the registry's hash order never reopens the gate — naming its
/// reference-bearing fields.
///
/// A collection or global without any reference field is left out: it
/// contributes nothing to any count, so adding or removing one must not
/// reopen every gate. Removing one that DID hold references still changes the
/// topology, because its entry disappears.
fn reference_topology(registry: &Registry) -> Vec<String> {
    let collections = registry
        .collections
        .iter()
        .map(|(slug, def)| ("collection", slug, reference_paths(&def.fields)));
    let globals = registry
        .globals
        .iter()
        .map(|(slug, def)| ("global", slug, reference_paths(&def.fields)));

    let mut topology: Vec<String> = collections
        .chain(globals)
        .filter(|(_, _, paths)| !paths.is_empty())
        .map(|(kind, slug, paths)| format!("{kind} {slug}: {paths}"))
        .collect();
    topology.sort_unstable();

    topology
}

/// Every relationship/upload leaf of `fields` at any depth — through groups,
/// arrays, blocks, layout wrappers and tabs — named by its path and described
/// by what decides which references it holds and where they are stored.
fn reference_paths(fields: &[FieldDefinition]) -> String {
    let mut paths = Vec::new();
    collect_reference_paths(fields, "", &mut paths);

    paths.join(",")
}

/// Push the reference leaves of `fields` under `prefix` onto `out`.
fn collect_reference_paths(fields: &[FieldDefinition], prefix: &str, out: &mut Vec<String>) {
    for field in fields {
        let path = field_path(prefix, field);

        match field_children(field) {
            FieldChildren::Leaf => out.extend(describe_reference(field).map(|d| path + &d)),
            FieldChildren::Group(sub) | FieldChildren::Array(sub) => {
                collect_reference_paths(sub, &path, out);
            }
            // Layout wrappers and tabs add no name, the way storage names don't.
            FieldChildren::Wrapper(sub) => collect_reference_paths(sub, prefix, out),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_reference_paths(&tab.fields, prefix, out);
                }
            }
            FieldChildren::Blocks(defs) => {
                for def in defs {
                    collect_reference_paths(
                        &def.fields,
                        &format!("{path}[{}]", def.block_type),
                        out,
                    );
                }
            }
        }
    }
}

/// `prefix.name`, marked `~l` when the field is localized: a localized field —
/// or a localized parent group — stores one column per locale, each counted.
fn field_path(prefix: &str, field: &FieldDefinition) -> String {
    let localized = if field.localized { "~l" } else { "" };

    if prefix.is_empty() {
        return format!("{}{localized}", field.name);
    }

    format!("{prefix}.{}{localized}", field.name)
}

/// A reference leaf's type, `[]` when it holds a list, and its targets (every
/// target of a polymorphic one, sorted); `None` for any other leaf.
fn describe_reference(field: &FieldDefinition) -> Option<String> {
    if !field.field_type.is_reference() {
        return None;
    }

    let rc = field.relationship.as_ref()?;

    let mut targets: Vec<&str> = if rc.polymorphic.is_empty() {
        vec![rc.collection.as_ref()]
    } else {
        rc.polymorphic.iter().map(AsRef::<str>::as_ref).collect()
    };
    targets.sort_unstable();

    let list = if rc.has_many || field.has_many {
        "[]"
    } else {
        ""
    };

    Some(format!(
        ":{}{list}->{}",
        field.field_type.as_str(),
        targets.join("|")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{BlockDefinition, CollectionDefinition, FieldType, RelationshipConfig, Slug},
        db::migrate::{
            backfill_ref_counts::test_support::{posts_with, registry_of, upload_to},
            collection::test_helpers::no_locale,
        },
    };

    fn gate_of(fields: Vec<FieldDefinition>) -> String {
        let registry = registry_of(&[
            CollectionDefinition::new("media"),
            CollectionDefinition::new("images"),
            posts_with(fields),
        ]);

        gate_value(&registry, &no_locale())
    }

    /// Removing a whole collection that held references reopens every gate:
    /// the references it held must stop counting on their targets.
    #[test]
    fn removing_a_referencing_collection_changes_the_gate() {
        let media = CollectionDefinition::new("media");
        let with = registry_of(&[media.clone(), posts_with(vec![upload_to("image", "media")])]);
        let without = registry_of(&[media]);

        assert_ne!(
            gate_value(&with, &no_locale()),
            gate_value(&without, &no_locale())
        );
    }

    /// Regression: the topology listed every collection and global, so adding
    /// or removing one that holds no reference at all reopened every gate and
    /// recounted the whole database for nothing.
    #[test]
    fn a_reference_free_collection_is_not_part_of_the_topology() {
        let media = CollectionDefinition::new("media");
        let posts = posts_with(vec![upload_to("image", "media")]);
        let notes = CollectionDefinition::new("notes");

        let without = registry_of(&[media.clone(), posts.clone()]);
        let with = registry_of(&[media, posts, notes]);

        assert_eq!(
            gate_value(&without, &no_locale()),
            gate_value(&with, &no_locale()),
            "a collection without references must not change the gate"
        );
    }

    /// Every change to what a reference field counts reopens the gate:
    /// removing it, retargeting it, turning it into a list, localizing it —
    /// at any depth.
    #[test]
    fn the_gate_follows_the_reference_topology() {
        let base = gate_of(vec![upload_to("image", "media")]);

        assert_ne!(base, gate_of(Vec::new()), "removed");
        assert_ne!(
            base,
            gate_of(vec![upload_to("image", "images")]),
            "retargeted"
        );
        assert_ne!(
            base,
            gate_of(vec![
                FieldDefinition::builder("image", FieldType::Upload)
                    .relationship(RelationshipConfig::new("media", true))
                    .build()
            ]),
            "turned into a list"
        );
        assert_ne!(
            base,
            gate_of(vec![
                FieldDefinition::builder("image", FieldType::Upload)
                    .relationship(RelationshipConfig::new("media", false))
                    .localized(true)
                    .build()
            ]),
            "localized"
        );
        assert_ne!(base, gate_of(vec![upload_to("cover", "media")]), "renamed");
    }

    /// A reference nested in a group, an array or a block is part of the
    /// topology, named by its path.
    #[test]
    fn nested_references_are_part_of_the_topology() {
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![upload_to("image", "media")])
            .build();
        let array = FieldDefinition::builder("slides", FieldType::Array)
            .fields(vec![upload_to("image", "media")])
            .build();
        let blocks = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "hero",
                vec![upload_to("image", "media")],
            )])
            .build();

        assert_eq!(reference_paths(&[group]), "meta.image:upload->media");
        assert_eq!(reference_paths(&[array]), "slides.image:upload->media");
        assert_eq!(
            reference_paths(&[blocks]),
            "content[hero].image:upload->media"
        );
    }

    /// A polymorphic field names all of its targets, in a stable order, and a
    /// scalar field is no part of the topology.
    #[test]
    fn polymorphic_targets_are_sorted_and_scalars_ignored() {
        let mut rc = RelationshipConfig::new("posts", true);
        rc.polymorphic = vec![Slug::new("posts"), Slug::new("pages")];
        let poly = FieldDefinition::builder("links", FieldType::Relationship)
            .relationship(rc)
            .build();
        let title = FieldDefinition::builder("title", FieldType::Text).build();

        assert_eq!(
            reference_paths(&[poly, title]),
            "links:relationship[]->pages|posts"
        );
        assert_eq!(
            gate_of(vec![upload_to("image", "media")]),
            gate_of(vec![
                upload_to("image", "media"),
                FieldDefinition::builder("title", FieldType::Text).build(),
            ]),
            "a scalar field must not reopen the gate"
        );
    }
}
