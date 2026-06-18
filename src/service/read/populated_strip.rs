//! Field-level read-access stripping for **populated** relationship targets.
//!
//! Collection-level read access on a relationship target is enforced during
//! populate (a denied target yields an empty array / null). But the embedded
//! document that populate inlines belongs to *another* collection with its own
//! field-level read access — and that was never applied, so a field a user is
//! denied on the target collection leaked through populate.
//!
//! This pass runs after assembly, walking the document for relationship/upload
//! values that populate inlined as embedded objects (each carries its source
//! `collection`), and strips each embedded object against *its own* collection's
//! field-read denials — recursively, so depth-N populated chains are covered.
//!
//! ## Cache safety
//!
//! The populate cache stores **full** target documents (field denials are
//! per-user, so they must not be baked into the shared cache). Stripping
//! therefore happens here, on the per-request assembled copy, never on the
//! cached document.
//!
//! ## Cost
//!
//! Gated on [`registry_has_read_field_controls`]: when no collection configures
//! any field-level read control (`access.read` or a hidden field) — the common
//! case — the whole pass is skipped with zero per-document work. Per-collection
//! denials are computed once and memoized across a batch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::core::{
    Document, FieldDefinition, FieldDenial, FieldType, JsonRoot, NestStep, Registry, VisitAction,
    any_field, walk_nested_mut,
};
use crate::service::{helpers::collect_api_hidden_field_names, hooks::ReadHooks};

/// Bound on cross-collection recursion. Populate breaks cycles (leaving ids), so
/// the assembled tree is finite and shallow; this is a generous backstop.
const MAX_EMBED_DEPTH: usize = 32;

/// Strips field-read-denied fields from populated relationship targets in read
/// output. Construct once per read (single or batch) and reuse across documents
/// so per-collection denials are computed at most once.
pub(crate) struct EmbeddedDocStripper<'a> {
    registry: &'a Registry,
    memo: DenialMemo<'a>,
    enabled: bool,
}

impl<'a> EmbeddedDocStripper<'a> {
    /// Build a stripper for the current user. Returns a disabled stripper (whose
    /// [`strip`](Self::strip) is a no-op) when no collection in the registry
    /// configures any field-level read control.
    pub(crate) fn new(
        registry: &'a Registry,
        hooks: &'a dyn ReadHooks,
        user: Option<&'a Document>,
        locale: Option<&'a str>,
    ) -> Self {
        Self {
            registry,
            memo: DenialMemo::new(hooks, user, locale),
            enabled: registry_has_read_field_controls(registry),
        }
    }

    /// Strip denied fields from every embedded relationship target in `doc`,
    /// where `fields` are the definitions of `doc`'s own collection.
    pub(crate) fn strip(&self, doc: &mut Document, fields: &[FieldDefinition]) {
        if !self.enabled {
            return;
        }

        strip_embedded(
            &mut doc.fields,
            fields,
            self.registry,
            &self.memo,
            MAX_EMBED_DEPTH,
        );
    }
}

/// Walk one document's fields against `fields`, stripping the embedded targets
/// of any relationship/upload leaf. The container recursion (group/array/blocks
/// at any depth) is the canonical [`walk_nested_mut`]'s; this only acts on
/// relationship leaves.
fn strip_embedded<R: JsonRoot + ?Sized>(
    root: &mut R,
    fields: &[FieldDefinition],
    registry: &Registry,
    memo: &DenialMemo<'_>,
    depth: usize,
) {
    if depth == 0 {
        return;
    }

    let mut path: Vec<NestStep<'_>> = Vec::new();
    walk_nested_mut(root, fields, &mut path, &mut |field, value, _path| {
        // The embedding leaf types — a populated value is an embedded doc (or
        // array of them) whose own field-read denials must be stripped. Join is
        // included: its populated value is an array of embedded target docs, so
        // a `hidden`/`access.read`-denied field on the join target would
        // otherwise leak (containers like group/array/blocks are descended into
        // by `walk_nested_mut`, not matched here).
        if !matches!(
            field.field_type,
            FieldType::Relationship | FieldType::Upload | FieldType::Join
        ) {
            return VisitAction::Keep;
        }

        let Some(value) = value else {
            return VisitAction::Keep;
        };

        match value {
            // Populated has-one: a single embedded object.
            Value::Object(obj) if is_embedded_ref(obj) => {
                let mut obj = obj.clone();
                strip_ref(&mut obj, registry, memo, depth);
                VisitAction::Replace(Value::Object(obj))
            }
            // Populated has-many: an array of embedded objects (unpopulated ids
            // stay strings and are left untouched).
            Value::Array(items) if items.iter().any(value_is_embedded_ref) => {
                let mut items = items.clone();
                for item in &mut items {
                    if let Value::Object(obj) = item
                        && is_embedded_ref(obj)
                    {
                        strip_ref(obj, registry, memo, depth);
                    }
                }
                VisitAction::Replace(Value::Array(items))
            }
            // Unpopulated (bare id string) or anything else — nothing to strip.
            _ => VisitAction::Keep,
        }
    });
}

/// Strip one embedded target object against its own collection's denials, then
/// recurse into *its* populated relationships (depth-bounded).
fn strip_ref(
    obj: &mut Map<String, Value>,
    registry: &Registry,
    memo: &DenialMemo<'_>,
    depth: usize,
) {
    let Some(collection) = obj.get("collection").and_then(Value::as_str) else {
        return;
    };
    let Some(def) = registry.get_collection(collection) else {
        return;
    };

    for denial in memo.denials_for(collection, &def.fields).iter() {
        denial.strip_from(obj);
    }

    strip_embedded(obj, &def.fields, registry, memo, depth - 1);
}

/// A populated relationship target carries both `collection` and `id` markers
/// (stamped by `document_to_json`). An unpopulated relationship is a bare id
/// string, so the (relationship-field + object-with-markers) pair is unambiguous.
fn is_embedded_ref(obj: &Map<String, Value>) -> bool {
    obj.contains_key("collection") && obj.contains_key("id")
}

fn value_is_embedded_ref(value: &Value) -> bool {
    value.as_object().is_some_and(is_embedded_ref)
}

/// Whether any collection in the registry configures a field-level read control
/// (`access.read` or a hidden field) at any nesting depth.
pub(crate) fn registry_has_read_field_controls(registry: &Registry) -> bool {
    registry
        .collections
        .values()
        .any(|def| any_field(&def.fields, &|f| f.hidden || f.access.read.is_some()))
}

/// Memoizes per-collection read denials (field access + api-hidden) for one
/// request's user, so the (Lua-evaluated) `field_read_denied` hook runs at most
/// once per collection.
struct DenialMemo<'a> {
    hooks: &'a dyn ReadHooks,
    user: Option<&'a Document>,
    locale: Option<&'a str>,
    cache: RefCell<HashMap<String, Arc<Vec<FieldDenial>>>>,
}

impl<'a> DenialMemo<'a> {
    fn new(hooks: &'a dyn ReadHooks, user: Option<&'a Document>, locale: Option<&'a str>) -> Self {
        Self {
            hooks,
            user,
            locale,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn denials_for(&self, collection: &str, fields: &[FieldDefinition]) -> Arc<Vec<FieldDenial>> {
        if let Some(denials) = self.cache.borrow().get(collection) {
            return Arc::clone(denials);
        }

        let mut denied = self.hooks.field_read_denied(fields, self.user, self.locale);
        denied.extend(collect_api_hidden_field_names(fields, ""));

        let denials = Arc::new(denied);
        self.cache
            .borrow_mut()
            .insert(collection.to_string(), Arc::clone(&denials));

        denials
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use serde_json::json;

    use super::*;
    use crate::core::{
        CollectionDefinition, FieldType, HookRef, Hooks, JoinConfig, RelationshipConfig,
    };
    use crate::db::AccessResult;
    use crate::hooks::{AccessCheckInput, lifecycle::AfterReadCtx};

    /// Mock that denies reads on any field carrying an `access.read` hook —
    /// mirroring the real `field_read_denied`, but without a live Lua VM.
    struct DenyAccessReadHooks;

    impl ReadHooks for DenyAccessReadHooks {
        fn before_read(
            &self,
            _hooks: &Hooks,
            _slug: &str,
            _op: &str,
            _locale: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _ctx: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn field_read_denied(
            &self,
            fields: &[FieldDefinition],
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) -> Vec<FieldDenial> {
            fields
                .iter()
                .filter(|f| f.access.read.is_some())
                .map(|f| FieldDenial::Flat(f.name.clone()))
                .collect()
        }
    }

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    /// A text field denied to readers via an `access.read` hook.
    fn denied(name: &str) -> FieldDefinition {
        let mut f = FieldDefinition::builder(name, FieldType::Text).build();
        f.access.read = Some(HookRef::new("deny"));
        f
    }

    fn rel(name: &str, target: &str, has_many: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new(target, has_many))
            .build()
    }

    fn collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new(slug);
        def.fields = fields;
        def
    }

    /// `authors` has a reader-denied `secret`; `posts.author` points at it.
    fn posts_and_authors() -> Registry {
        let authors = collection(
            "authors",
            vec![
                text("name"),
                denied("secret"),
                rel("mentor", "authors", false),
            ],
        );
        let posts = collection(
            "posts",
            vec![text("title"), rel("author", "authors", false)],
        );

        let mut registry = Registry::new();
        registry.register_collection(authors);
        registry.register_collection(posts);
        registry
    }

    fn strip_post(registry: &Registry, doc: &mut Document) {
        let hooks = DenyAccessReadHooks;
        EmbeddedDocStripper::new(registry, &hooks, None, None).strip(
            doc,
            &registry.get_collection("posts").unwrap().fields.clone(),
        );
    }

    #[test]
    fn strips_denied_field_from_populated_has_one_target() {
        let registry = posts_and_authors();
        let mut doc = Document::new("p1".to_string());
        doc.fields.insert(
            "author".into(),
            json!({ "id": "a1", "collection": "authors", "name": "Ada", "secret": "xyz" }),
        );

        strip_post(&registry, &mut doc);

        let author = doc.fields.get("author").unwrap();
        assert_eq!(author.get("name").and_then(|v| v.as_str()), Some("Ada"));
        assert!(
            author.get("secret").is_none(),
            "reader-denied field on the target collection must not leak via populate"
        );
    }

    #[test]
    fn strips_denied_field_from_every_populated_has_many_target() {
        let registry = posts_and_authors();
        // Reuse the has-one fixture but as a has-many `author` value.
        let mut doc = Document::new("p1".to_string());
        doc.fields.insert(
            "author".into(),
            json!([
                { "id": "a1", "collection": "authors", "name": "Ada", "secret": "x" },
                "a2",
                { "id": "a3", "collection": "authors", "name": "Bo", "secret": "y" }
            ]),
        );

        strip_post(&registry, &mut doc);

        let arr = doc.fields.get("author").unwrap().as_array().unwrap();
        assert!(arr[0].get("secret").is_none(), "first target stripped");
        assert_eq!(arr[1].as_str(), Some("a2"), "unpopulated id left untouched");
        assert!(arr[2].get("secret").is_none(), "third target stripped");
    }

    #[test]
    fn strips_denied_field_at_depth_two() {
        let registry = posts_and_authors();
        let mut doc = Document::new("p1".to_string());
        doc.fields.insert(
            "author".into(),
            json!({
                "id": "a1", "collection": "authors", "name": "Ada", "secret": "x",
                "mentor": { "id": "a2", "collection": "authors", "name": "Bo", "secret": "deep" }
            }),
        );

        strip_post(&registry, &mut doc);

        let author = doc.fields.get("author").unwrap();
        assert!(author.get("secret").is_none(), "depth-1 secret stripped");
        let mentor = author.get("mentor").unwrap();
        assert_eq!(mentor.get("name").and_then(|v| v.as_str()), Some("Bo"));
        assert!(
            mentor.get("secret").is_none(),
            "depth-2 nested populated target's secret must also be stripped"
        );
    }

    /// Regression: a JOIN-typed field's populated targets must also be stripped.
    /// Join was missing from the strip set, so a reader-denied field on the join
    /// target leaked while the identical field on a relationship target did not.
    #[test]
    fn strips_denied_field_from_populated_join_target() {
        let posts = collection("posts", vec![text("title"), denied("secret")]);
        let mut authors = collection("authors", vec![text("name")]);
        authors.fields.push(
            FieldDefinition::builder("recent_posts", FieldType::Join)
                .join(JoinConfig::new("posts", "author"))
                .build(),
        );
        let mut registry = Registry::new();
        registry.register_collection(posts);
        registry.register_collection(authors);

        let mut doc = Document::new("au1".to_string());
        doc.fields.insert(
            "recent_posts".into(),
            json!([
                { "id": "p1", "collection": "posts", "title": "A", "secret": "x" },
                { "id": "p2", "collection": "posts", "title": "B", "secret": "y" }
            ]),
        );

        let hooks = DenyAccessReadHooks;
        EmbeddedDocStripper::new(&registry, &hooks, None, None).strip(
            &mut doc,
            &registry.get_collection("authors").unwrap().fields.clone(),
        );

        let arr = doc.fields.get("recent_posts").unwrap().as_array().unwrap();
        assert_eq!(arr[0].get("title").and_then(|v| v.as_str()), Some("A"));
        assert!(
            arr[0].get("secret").is_none(),
            "reader-denied field on a JOIN target must not leak via populate"
        );
        assert!(
            arr[1].get("secret").is_none(),
            "every join target must be stripped"
        );
    }

    /// Security: a Join nested inside a Group (now populated) must still have
    /// its target's reader-denied fields stripped — the strip pass descends the
    /// group and treats the Join leaf as an embedding type.
    #[test]
    fn strips_denied_field_from_join_nested_in_group() {
        let posts = collection("posts", vec![text("title"), denied("secret")]);
        let mut authors = collection("authors", vec![]);
        let mut group = FieldDefinition::builder("section", FieldType::Group).build();
        group.fields = vec![
            FieldDefinition::builder("recent", FieldType::Join)
                .join(JoinConfig::new("posts", "author"))
                .build(),
        ];
        authors.fields.push(group);
        let mut registry = Registry::new();
        registry.register_collection(posts);
        registry.register_collection(authors);

        let mut doc = Document::new("au1".to_string());
        doc.fields.insert(
            "section".into(),
            json!({
                "recent": [
                    { "id": "p1", "collection": "posts", "title": "A", "secret": "x" }
                ]
            }),
        );

        let hooks = DenyAccessReadHooks;
        EmbeddedDocStripper::new(&registry, &hooks, None, None).strip(
            &mut doc,
            &registry.get_collection("authors").unwrap().fields.clone(),
        );

        let arr = doc
            .fields
            .get("section")
            .and_then(|s| s.get("recent"))
            .and_then(|v| v.as_array())
            .expect("nested join array present");
        assert_eq!(arr[0].get("title").and_then(|v| v.as_str()), Some("A"));
        assert!(
            arr[0].get("secret").is_none(),
            "a denied field on a Join target nested in a group must be stripped"
        );
    }

    #[test]
    fn pass_is_skipped_when_no_collection_has_read_controls() {
        // No field carries access.read or hidden → gate disables the pass, so
        // even an embedded object is left exactly as-is.
        let authors = collection("authors", vec![text("name")]);
        let posts = collection("posts", vec![rel("author", "authors", false)]);
        let mut registry = Registry::new();
        registry.register_collection(authors);
        registry.register_collection(posts);

        assert!(!registry_has_read_field_controls(&registry));

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert(
            "author".into(),
            json!({ "id": "a1", "collection": "authors", "name": "Ada" }),
        );

        strip_post(&registry, &mut doc);

        assert_eq!(
            doc.fields
                .get("author")
                .unwrap()
                .get("name")
                .and_then(|v| v.as_str()),
            Some("Ada")
        );
    }
}
