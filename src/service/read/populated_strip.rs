//! Read processing for **populated** relationship and join targets.
//!
//! Collection-level read access on a relationship target is enforced during
//! populate (a denied target yields an empty array / null). But the embedded
//! document that populate inlines belongs to *another* collection, with its own
//! field-level read access and its own read hooks. This pass applies both, so an
//! embedded document reads exactly as a direct read of it would:
//!
//! 1. its collection's `before_read` hooks run (once per target collection per
//!    read); an aborting hook hides the collection's embedded documents, as a
//!    denied read would;
//! 2. its field-read denials are stripped — data-aware `access.read` (the target
//!    document is its own `ctx.document`) and the `hidden` fields;
//! 3. its own populated targets are processed the same way, recursively;
//! 4. its collection's `after_read` hooks run (field level, then collection
//!    level, then registered), with the reader's user, locale and the read's
//!    operation, fail-open like every `after_read` — batched: one call per
//!    target collection per embedding level across the whole read, deepest
//!    level first, so a document's hooks see its own embedded targets already
//!    processed (see [`after_read`]).
//!
//! ## Which collection an embedded document belongs to
//!
//! Decided by the relationship / join **field definition**, never by document
//! data: a join's or a non-polymorphic relationship's one target, or — for a
//! polymorphic relationship — the populated reference's collection when the
//! field allows it. The `collection` tag populate stamps is the server's own
//! (`document_to_json` writes it after the document's fields, and `collection`
//! is a reserved field name); it only selects among the field's targets. An
//! embedded document whose collection cannot be resolved that way is dropped
//! (null for a has-one), never passed through unprocessed.
//!
//! ## Cache safety
//!
//! The populate cache stores **full** target documents (field denials are
//! per-user, so they must not be baked into the shared cache). Processing
//! therefore happens here, on the per-request assembled copy, never on the
//! cached document.
//!
//! ## Cost
//!
//! Runs only for reads that populate (`depth > 0`). Per-collection hidden-field
//! denials and `before_read` outcomes are computed once per read and memoized;
//! `after_read` takes one Lua VM lease per target collection per level, and
//! none for a collection without read hooks.

mod after_read;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    slice,
    sync::Arc,
};

use serde_json::{Map, Value};
use tracing::warn;

use crate::{
    core::{
        Builder, CollectionDefinition, Document, DocumentFields, FieldDefinition, FieldDenial,
        FieldType, JoinConfig, JsonRoot, NestStep, Registry, ReqContext, VisitAction,
        walk_nested_mut,
    },
    hooks::lifecycle::access::has_any_field_access,
    service::{helpers::collect_api_hidden_field_names, hooks::ReadHooks},
};

/// Bound on cross-collection recursion. Populate breaks cycles (leaving ids), so
/// the assembled tree is finite; this is a generous backstop. An embedded
/// document below it is dropped, never passed through unprocessed.
const MAX_EMBED_DEPTH: usize = 32;

/// The envelope key populate tags an embedded target with.
const TAG_KEY: &str = "collection";

/// Who reads the embedding document, and how — the context every embedded
/// target is processed under, exactly as a direct read of it would be.
#[derive(Builder)]
pub(crate) struct EmbeddedReader<'a> {
    #[builder(required)]
    registry: &'a Registry,
    #[builder(required)]
    hooks: &'a dyn ReadHooks,
    /// The embedding read's operation (`find` / `find_by_id`); the embedded
    /// targets' hooks see the read they are part of.
    #[builder(required)]
    operation: &'a str,
    user: Option<&'a Document>,
    /// Locale field-read access and `before_read` are evaluated for.
    access_locale: Option<&'a str>,
    /// Locale `after_read` hooks see (`None` for an all-locales read).
    hook_locale: Option<&'a str>,
    ui_locale: Option<&'a str>,
}

/// Processes the populated relationship / join targets of read output (see the
/// module docs). Construct once per read (single or batch) and reuse across
/// documents: document-independent work is memoized per collection, while the
/// data-aware `access.read` strip is evaluated per embedded target.
pub(crate) struct EmbeddedDocPass<'a> {
    reader: EmbeddedReader<'a>,
    /// Per-collection API-hidden denials (document-independent — safe to memoize).
    api_hidden: RefCell<HashMap<String, Arc<Vec<FieldDenial>>>>,
    /// Per-collection `before_read` outcome: the shared context its `after_read`
    /// hooks see, or `None` when a hook refused the read.
    read_contexts: RefCell<HashMap<String, Option<ReqContext>>>,
    /// The deepest embedding level the current pass kept a target with
    /// `after_read` hooks at (1 = a target embedded in a read document), so
    /// the batches know how many levels to run; 0 runs none.
    deepest: Cell<usize>,
}

impl<'a> EmbeddedDocPass<'a> {
    pub(crate) fn new(reader: EmbeddedReader<'a>) -> Self {
        Self {
            reader,
            api_hidden: RefCell::new(HashMap::new()),
            read_contexts: RefCell::new(HashMap::new()),
            deepest: Cell::new(0),
        }
    }

    /// Process every embedded target in `doc`, where `fields` are the
    /// definitions of `doc`'s own collection.
    pub(crate) fn process(&self, doc: &mut Document, fields: &[FieldDefinition]) {
        self.process_many(slice::from_mut(doc), fields);
    }

    /// Process every embedded target in `docs`, documents of one collection
    /// whose definitions are `fields`: strip and resolve every target, then
    /// run the `after_read` hooks in batches (see [`after_read`]).
    pub(crate) fn process_many(&self, docs: &mut [Document], fields: &[FieldDefinition]) {
        self.deepest.set(0);

        for doc in docs.iter_mut() {
            self.process_embedded(&mut doc.fields, fields, MAX_EMBED_DEPTH);
        }

        for level in (1..=self.deepest.get()).rev() {
            self.run_after_read_level(docs, fields, level);
        }
    }

    /// Walk one document's fields against `fields`, processing the embedded
    /// targets of any relationship/upload/join leaf. The container recursion
    /// (group/array/blocks at any depth) is the canonical [`walk_nested_mut`]'s;
    /// this only acts on the embedding leaves.
    fn process_embedded<R: JsonRoot>(
        &self,
        root: &mut R,
        fields: &[FieldDefinition],
        depth: usize,
    ) {
        let mut path: Vec<NestStep<'_>> = Vec::new();

        walk_nested_mut(root, fields, &mut path, &mut |field, level, _path| {
            if !is_embedding(field) {
                return VisitAction::Keep;
            }

            let Some(value) = level.root_get(&field.name) else {
                return VisitAction::Keep;
            };

            self.process_value(value, field, depth)
        });
    }

    /// The processed value of one embedding field: a populated has-one (one
    /// embedded object, `null` when it must not be shown), a populated has-many
    /// or join (embedded objects dropped when they must not be shown), or an
    /// unpopulated value left as it is.
    fn process_value(&self, value: &Value, field: &FieldDefinition, depth: usize) -> VisitAction {
        match value {
            Value::Object(obj) if is_embedded_ref(obj) => {
                let processed = self.process_ref(obj.clone(), field, depth);

                VisitAction::Replace(processed.unwrap_or(Value::Null))
            }
            Value::Array(items) if items.iter().any(value_is_embedded_ref) => {
                VisitAction::Replace(Value::Array(self.process_items(items, field, depth)))
            }
            _ => VisitAction::Keep,
        }
    }

    /// Process every embedded target of a populated has-many or join array;
    /// unpopulated ids stay as they are.
    fn process_items(&self, items: &[Value], field: &FieldDefinition, depth: usize) -> Vec<Value> {
        items
            .iter()
            .filter_map(|item| match item {
                Value::Object(obj) if is_embedded_ref(obj) => {
                    self.process_ref(obj.clone(), field, depth)
                }
                other => Some(other.clone()),
            })
            .collect()
    }

    /// Process one embedded target of `field`: resolve its collection from the
    /// field, run that collection's `before_read`, strip its field-read denials
    /// and process its own embedded targets; its `after_read` runs later, in
    /// its level's batch. `None` when the target must not be shown — its
    /// collection unresolvable, its `before_read` refusing, (a join child) its
    /// `on` value unreadable, or the recursion bound reached.
    fn process_ref(
        &self,
        mut obj: Map<String, Value>,
        field: &FieldDefinition,
        depth: usize,
    ) -> Option<Value> {
        if depth == 0 {
            return None;
        }

        let def = self.embedded_target(field, &obj)?;
        self.read_context(def)?;

        self.strip_ref(&mut obj, def);

        if let Some(join) = &field.join
            && !join_child_readable(join, &obj)
        {
            return None;
        }

        self.process_embedded(&mut obj, &def.fields, depth - 1);

        self.note_after_read(def, depth);

        Some(Value::Object(obj))
    }

    /// Record that a target of `def` was kept at recursion `depth`, when its
    /// collection has `after_read` hooks to run: the batches then run down to
    /// its level. A read embedding only hook-less collections runs none.
    fn note_after_read(&self, def: &CollectionDefinition, depth: usize) {
        if !self.reader.hooks.wants_after_read(&def.hooks, &def.fields) {
            return;
        }

        let level = MAX_EMBED_DEPTH - depth + 1;
        self.deepest.set(self.deepest.get().max(level));
    }

    /// The collection an embedded target of `field` belongs to, judged by the
    /// field definition (see the module docs). `None` fails closed.
    fn embedded_target(
        &self,
        field: &FieldDefinition,
        obj: &Map<String, Value>,
    ) -> Option<&'a CollectionDefinition> {
        let tag = obj.get(TAG_KEY).and_then(Value::as_str)?;

        if !field_targets(field).contains(&tag) {
            return None;
        }

        self.reader.registry.get_collection(tag).map(Arc::as_ref)
    }

    /// The `before_read` outcome for `def`'s embedded documents, run once per
    /// collection per read: the shared context for its `after_read`, or `None`
    /// when a hook aborted the read.
    fn read_context(&self, def: &CollectionDefinition) -> Option<ReqContext> {
        if let Some(outcome) = self.read_contexts.borrow().get(def.slug.as_ref()) {
            return outcome.clone();
        }

        let reader = &self.reader;
        let outcome = reader
            .hooks
            .before_read(
                &def.hooks,
                &def.slug,
                reader.operation,
                reader.access_locale,
            )
            .inspect_err(|e| {
                warn!(
                    "before_read on {} refused its populated documents: {e:#}",
                    def.slug
                );
            })
            .ok();

        self.read_contexts
            .borrow_mut()
            .insert(def.slug.to_string(), outcome.clone());

        outcome
    }

    /// Strip one embedded target against its own collection's denials —
    /// data-aware `access.read` (the target doc is its own `ctx.document`) plus
    /// the static API-hidden set.
    fn strip_ref(&self, obj: &mut Map<String, Value>, def: &CollectionDefinition) {
        // Skip the per-target document clone unless the collection actually
        // configures `access.read`.
        if has_any_field_access(&def.fields, |f| f.access.read.as_ref()) {
            let document: DocumentFields = obj.clone().into_iter().collect();

            self.reader.hooks.strip_read_access_map(
                &def.fields,
                obj,
                &document,
                &def.slug,
                self.reader.user,
                self.reader.access_locale,
            );
        }

        for denial in self.api_hidden_for(def).iter() {
            denial.strip_from(obj);
        }
    }

    /// Memoized per-collection API-hidden denials (document-independent), so the
    /// `hidden`-field name walk runs at most once per collection per read.
    fn api_hidden_for(&self, def: &CollectionDefinition) -> Arc<Vec<FieldDenial>> {
        if let Some(denials) = self.api_hidden.borrow().get(def.slug.as_ref()) {
            return Arc::clone(denials);
        }

        let denials = Arc::new(collect_api_hidden_field_names(&def.fields, ""));

        self.api_hidden
            .borrow_mut()
            .insert(def.slug.to_string(), Arc::clone(&denials));

        denials
    }
}

/// The field types whose populated value is an embedded document (or an array
/// of them). Containers (group/array/blocks) are descended into by
/// [`walk_nested_mut`], not matched here.
fn is_embedding(field: &FieldDefinition) -> bool {
    matches!(
        field.field_type,
        FieldType::Relationship | FieldType::Upload | FieldType::Join
    )
}

/// The collections a populated value of `field` may belong to, per its
/// definition: a join's target, or a relationship's (every polymorphic) target.
fn field_targets(field: &FieldDefinition) -> Vec<&str> {
    if let Some(join) = &field.join {
        return vec![join.collection.as_ref()];
    }

    field
        .relationship
        .as_ref()
        .map(|rel| rel.all_collections())
        .unwrap_or_default()
}

/// An embedded target as the document its hooks see: the envelope keys (`id`,
/// the tag, the timestamps) lifted out of the field map.
fn embedded_to_document(mut obj: Map<String, Value>) -> Document {
    let mut take = |key: &str| obj.remove(key).and_then(|v| v.as_str().map(str::to_string));

    let id = take("id").unwrap_or_default();
    let created_at = take("created_at");
    let updated_at = take("updated_at");

    obj.remove(TAG_KEY);

    Document::builder(id)
        .fields(obj.into_iter().collect::<DocumentFields>())
        .created_at(created_at)
        .updated_at(updated_at)
        .build()
}

/// Whether a join lists `child` — one of its target documents, already
/// stripped of what the viewer may not read. A join's children are exactly
/// the documents whose `on` field holds this document, so listing a child
/// whose `on` value the viewer may not read (a `hidden` field, or an
/// `access.read` rule denying it for that child) would reveal that value. The
/// one rule behind every join listing: the populated join array and the admin
/// join field's items and count.
pub(crate) fn join_child_readable<R: JsonRoot + ?Sized>(join: &JoinConfig, child: &R) -> bool {
    child.root_get(&join.on).is_some()
}

/// A populated relationship target carries both `collection` and `id` markers
/// (stamped by `document_to_json`). An unpopulated relationship is a bare id
/// string, so the (relationship-field + object-with-markers) pair is unambiguous.
fn is_embedded_ref(obj: &Map<String, Value>) -> bool {
    obj.contains_key(TAG_KEY) && obj.contains_key("id")
}

fn value_is_embedded_ref(value: &Value) -> bool {
    value.as_object().is_some_and(is_embedded_ref)
}

#[cfg(test)]
mod tests;
