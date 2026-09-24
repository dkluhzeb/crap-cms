//! Every relationship, upload and join field names a target collection; a
//! target that is not registered is a schema error, not a runtime surprise.
//!
//! Without this pass a dangling target (a typo, a removed collection) booted
//! and then failed wherever the target table is touched — the ref-count
//! recompute at startup, every write that adjusts reference counts, populate,
//! delete protection — and the client type generators emitted a reference to a
//! type that does not exist. An upload field must additionally target an
//! upload collection: the admin picker, thumbnails and the file URL shape all
//! read the target's upload metadata. A join field must additionally name, in
//! `on`, a field of its target that references the collection owning the join
//! — otherwise it booted and silently populated nothing.

use anyhow::{Result, bail};

use crate::{
    core::{FieldDefinition, FieldType, JoinConfig, Registry, find_field, walk_all_fields},
    hooks::startup_checks::hook_refs::field_source_label,
};

/// Reject every relationship/upload/join field — at any depth, in any
/// collection or global — whose target is not a registered collection
/// (polymorphic relationships: every listed target), every upload field
/// whose target is not an upload collection, and every join field whose `on`
/// does not reference the collection owning it (see [`join_on_problem`]).
///
/// # Errors
///
/// Returns an aggregated error naming each offending definition, field path
/// and target.
pub fn validate_relation_targets(registry: &Registry) -> Result<()> {
    let mut offenders = Vec::new();

    for (slug, def) in &registry.collections {
        let owner = Owner {
            label: format!("collection '{slug}'"),
            collection: Some(slug),
        };

        check_fields(registry, &def.fields, &owner, &mut offenders);
    }

    for (slug, def) in &registry.globals {
        let owner = Owner {
            label: format!("global '{slug}'"),
            collection: None,
        };

        check_fields(registry, &def.fields, &owner, &mut offenders);
    }

    if offenders.is_empty() {
        return Ok(());
    }

    // Registry maps iterate in arbitrary order; sort for a stable message.
    offenders.sort();

    bail!(
        "Field targets a collection that is not usable as its target:\n  - {}",
        offenders.join("\n  - ")
    )
}

/// The definition whose fields are checked: its label in messages, and its
/// slug when it is a collection (a join points back at it).
struct Owner<'a> {
    label: String,
    collection: Option<&'a str>,
}

/// Walk one definition's field tree (groups, arrays, blocks, tabs, rows,
/// collapsibles) and record every bad target.
fn check_fields(
    registry: &Registry,
    fields: &[FieldDefinition],
    owner: &Owner<'_>,
    out: &mut Vec<String>,
) {
    walk_all_fields(fields, &mut Vec::new(), &mut |field, path| {
        for problem in target_problems(registry, field, owner.collection) {
            out.push(format!(
                "{}: {problem}",
                field_source_label(&owner.label, path, field)
            ));
        }
    });
}

/// The problems with one field's targets (empty for a field that has none).
/// `owner` is the collection the field belongs to (`None` in a global).
fn target_problems(
    registry: &Registry,
    field: &FieldDefinition,
    owner: Option<&str>,
) -> Vec<String> {
    if let Some(join) = &field.join {
        return missing_target(registry, &join.collection)
            .or_else(|| join_on_problem(registry, join, owner))
            .into_iter()
            .collect();
    }

    let Some(rel) = &field.relationship else {
        return Vec::new();
    };

    rel.all_collections()
        .into_iter()
        .filter_map(|target| {
            missing_target(registry, target).or_else(|| non_upload_target(registry, field, target))
        })
        .collect()
}

fn missing_target(registry: &Registry, target: &str) -> Option<String> {
    registry
        .get_collection(target)
        .is_none()
        .then(|| format!("target collection '{target}' is not defined"))
}

/// The problem with a join's `on`, if any. The join lists the target's
/// documents whose `on` column holds the owning document's id, so `on` must
/// name a has-one, single-target relationship or upload field of the target —
/// at its top level, layout wrappers transparent — whose target is the
/// collection owning the join. That is the one shape the join populate reads,
/// on both the single-document and the list path (the list path groups the
/// matches by the target document's own `on` value); any other `on` populated
/// nothing. A global is never referenced, so a join in a global never
/// matches either.
fn join_on_problem(registry: &Registry, join: &JoinConfig, owner: Option<&str>) -> Option<String> {
    let Some(owner) = owner else {
        return Some(
            "a join lists the documents that reference its own document, and nothing \
             can reference a global"
                .to_string(),
        );
    };

    let target = registry.get_collection(&join.collection)?;
    let on = &join.on;

    let points_back = find_field(on, &target.fields).is_some_and(|field| references(field, owner));

    (!points_back).then(|| {
        format!(
            "join `on` '{on}' must name a top-level has-one relationship or upload field \
             of '{}' that references '{owner}'",
            join.collection
        )
    })
}

/// Whether `field` is a has-one, single-target relationship or upload field
/// whose target is `collection`.
fn references(field: &FieldDefinition, collection: &str) -> bool {
    if !matches!(
        field.field_type,
        FieldType::Relationship | FieldType::Upload
    ) {
        return false;
    }

    field
        .relationship
        .as_ref()
        .is_some_and(|rel| !rel.has_many && !rel.is_polymorphic() && &*rel.collection == collection)
}

fn non_upload_target(registry: &Registry, field: &FieldDefinition, target: &str) -> Option<String> {
    if field.field_type != FieldType::Upload {
        return None;
    }

    let def = registry.get_collection(target)?;

    (!def.is_upload_collection())
        .then(|| format!("upload field targets '{target}', which is not an upload collection"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        BlockDefinition, CollectionDefinition, GlobalDefinition, RelationshipConfig,
        upload::CollectionUpload,
    };

    fn rel(name: &str, target: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new(target, false))
            .build()
    }

    fn upload(name: &str, target: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Upload)
            .relationship(RelationshipConfig::new(target, false))
            .build()
    }

    fn collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.fields = fields;
        def
    }

    fn media() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());
        def
    }

    fn registry(defs: Vec<CollectionDefinition>) -> Registry {
        let mut registry = Registry::new();

        for def in defs {
            registry.register_collection(def);
        }

        registry
    }

    fn error_of(registry: &Registry) -> String {
        format!(
            "{:#}",
            validate_relation_targets(registry).expect_err("a bad target must fail the load")
        )
    }

    #[test]
    fn registered_targets_pass() {
        let reg = registry(vec![
            collection(
                "posts",
                vec![rel("author", "users"), upload("hero", "media")],
            ),
            collection("users", vec![]),
            media(),
        ]);

        assert!(validate_relation_targets(&reg).is_ok());
    }

    /// Regression: a relationship to an unregistered collection booted, then
    /// failed the ref-count recompute ("relation does not exist") and every
    /// write that adjusts reference counts.
    #[test]
    fn a_top_level_dangling_target_fails_naming_field_and_target() {
        let reg = registry(vec![collection("posts", vec![rel("author", "userz")])]);

        let err = error_of(&reg);
        let expected =
            "collection 'posts' field 'author': target collection 'userz' is not defined";
        assert!(err.contains(expected), "{err}");
    }

    #[test]
    fn nested_dangling_targets_are_found_at_any_depth() {
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![rel("owner", "ghosts")])
            .build();
        let array = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![rel("related", "phantoms")])
            .build();
        let blocks = FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "card",
                vec![rel("link", "spectres")],
            )])
            .build();
        let row = FieldDefinition::builder("row", FieldType::Row)
            .fields(vec![rel("buddy", "wraiths")])
            .build();
        let reg = registry(vec![collection("posts", vec![group, array, blocks, row])]);

        let err = error_of(&reg);
        for needle in [
            "field 'meta' field 'owner': target collection 'ghosts'",
            "field 'items' field 'related': target collection 'phantoms'",
            "field 'content' block 'card' field 'link': target collection 'spectres'",
            "field 'row' field 'buddy': target collection 'wraiths'",
        ] {
            assert!(err.contains(needle), "missing `{needle}` in {err}");
        }
    }

    #[test]
    fn every_polymorphic_target_must_exist() {
        let mut config = RelationshipConfig::new("users", false);
        config.polymorphic = vec!["users".into(), "teams".into()];
        let field = FieldDefinition::builder("owner", FieldType::Relationship)
            .relationship(config)
            .build();
        let reg = registry(vec![
            collection("posts", vec![field]),
            collection("users", vec![]),
        ]);

        let err = error_of(&reg);
        assert!(
            err.contains("target collection 'teams' is not defined"),
            "{err}"
        );
        assert!(!err.contains("'users' is not defined"), "{err}");
    }

    #[test]
    fn a_join_to_an_unknown_collection_fails() {
        let reg = registry(vec![collection(
            "users",
            vec![join("posts", "postz", "author")],
        )]);

        let err = error_of(&reg);
        let expected = "collection 'users' field 'posts': target collection 'postz' is not defined";
        assert!(err.contains(expected), "{err}");
    }

    fn join(name: &str, target: &str, on: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Join)
            .join(JoinConfig::new(target, on))
            .build()
    }

    /// `users` with a join over `posts` on `on`, and `posts` holding `fields`.
    fn join_registry(on: &str, post_fields: Vec<FieldDefinition>) -> Registry {
        registry(vec![
            collection("users", vec![join("posts", "posts", on)]),
            collection("posts", post_fields),
            collection("teams", vec![]),
        ])
    }

    #[test]
    fn a_join_on_a_back_reference_passes() {
        let reg = join_registry("author", vec![rel("author", "users")]);

        assert!(validate_relation_targets(&reg).is_ok());
    }

    #[test]
    fn a_join_on_a_back_reference_inside_a_layout_wrapper_passes() {
        let row = FieldDefinition::builder("row", FieldType::Row)
            .fields(vec![rel("author", "users")])
            .build();
        let reg = join_registry("author", vec![row]);

        assert!(validate_relation_targets(&reg).is_ok());
    }

    #[test]
    fn a_join_on_an_upload_back_reference_passes() {
        let mut uploads = media();
        uploads.fields = vec![join("used_in", "posts", "hero")];

        let reg = registry(vec![
            uploads,
            collection("posts", vec![upload("hero", "media")]),
        ]);

        assert!(validate_relation_targets(&reg).is_ok());
    }

    /// A join nested in a row of the owning collection still lists the
    /// documents referencing that collection's document.
    #[test]
    fn a_nested_join_points_back_at_its_owning_collection() {
        let items = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![join("posts", "posts", "author")])
            .build();
        let reg = registry(vec![
            collection("users", vec![items]),
            collection("posts", vec![rel("author", "users")]),
        ]);

        assert!(validate_relation_targets(&reg).is_ok());
    }

    /// Regression: a join whose `on` did not reference the owning collection
    /// booted and silently populated nothing.
    #[test]
    fn a_join_on_that_does_not_reference_the_owner_fails() {
        let mut polymorphic = RelationshipConfig::new("users", false);
        polymorphic.polymorphic = vec!["users".into(), "teams".into()];

        let cases = [
            ("nope", vec![rel("author", "users")]),
            ("author", vec![rel("author", "teams")]),
            ("author", vec![text_field("author")]),
            (
                "author",
                vec![
                    FieldDefinition::builder("author", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("users", true))
                        .build(),
                ],
            ),
            (
                "author",
                vec![
                    FieldDefinition::builder("author", FieldType::Relationship)
                        .relationship(polymorphic)
                        .build(),
                ],
            ),
            (
                "meta__author",
                vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![rel("author", "users")])
                        .build(),
                ],
            ),
        ];

        for (on, post_fields) in cases {
            let err = error_of(&join_registry(on, post_fields));
            let expected = format!(
                "collection 'users' field 'posts': join `on` '{on}' must name a top-level \
                 has-one relationship or upload field of 'posts' that references 'users'"
            );

            assert!(err.contains(&expected), "{on}: {err}");
        }
    }

    #[test]
    fn a_join_in_a_global_fails() {
        let mut reg = registry(vec![collection("posts", vec![rel("author", "users")])]);
        let mut global = GlobalDefinition::new("site");
        global.fields = vec![join("posts", "posts", "author")];
        reg.register_global(global);

        let err = error_of(&reg);
        assert!(
            err.contains("global 'site' field 'posts': a join lists the documents"),
            "{err}"
        );
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    #[test]
    fn a_global_field_with_a_dangling_target_fails() {
        let mut reg = registry(vec![]);
        let mut global = GlobalDefinition::new("site");
        global.fields = vec![upload("logo", "media")];
        reg.register_global(global);

        let err = error_of(&reg);
        let expected = "global 'site' field 'logo': target collection 'media' is not defined";
        assert!(err.contains(expected), "{err}");
    }

    #[test]
    fn an_upload_field_must_target_an_upload_collection() {
        let reg = registry(vec![
            collection("posts", vec![upload("hero", "images")]),
            collection("images", vec![]),
        ]);

        let err = error_of(&reg);
        assert!(
            err.contains("upload field targets 'images', which is not an upload collection"),
            "{err}"
        );
    }
}
