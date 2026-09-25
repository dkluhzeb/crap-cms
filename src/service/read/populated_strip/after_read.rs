//! The `after_read` hooks of embedded targets, batched.
//!
//! Every embedded target runs its own collection's `after_read` hooks. Called
//! per target, each call took its own Lua VM lease; a list read embedding
//! hundreds of targets took hundreds. Instead the targets are gathered per
//! **embedding level** (1 = embedded in a read document, 2 = embedded in one
//! of those, …) and per **collection**, and each group runs through one
//! `after_read_many` call — one lease — in the order the targets appear.
//!
//! Levels run deepest first, so a target's hooks see its own embedded targets
//! already processed, exactly as the per-target recursion did. Each target
//! runs under its collection's shared `before_read` context with its own
//! instruction budget, as the documents of one list read do.

use std::{collections::HashMap, vec};

use serde_json::{Map, Value};

use super::{
    EmbeddedDocPass, embedded_to_document, is_embedded_ref, is_embedding, value_is_embedded_ref,
};
use crate::{
    core::{
        CollectionDefinition, Document, FieldDefinition, JsonRoot, NestStep, VisitAction,
        walk_nested_mut,
    },
    db::query::populate::document_to_json,
    hooks::lifecycle::AfterReadCtx,
};

/// A visitor over the embedded targets of one level: the target's collection
/// and its value, which it may replace.
type LevelVisitor<'v, 'a> = dyn FnMut(&'a CollectionDefinition, &mut Value) + 'v;

/// Where a level walk stands: the level it looks for and the level of the
/// targets directly under the current object.
#[derive(Clone, Copy)]
struct LevelPos {
    target: usize,
    current: usize,
}

impl LevelPos {
    fn new(target: usize, current: usize) -> Self {
        Self { target, current }
    }

    /// The position one embedding deeper.
    fn deeper(self) -> Self {
        Self::new(self.target, self.current + 1)
    }
}

impl<'a> EmbeddedDocPass<'a> {
    /// Run the `after_read` hooks of every target kept at `level` in `docs`
    /// (documents of the collection whose definitions are `fields`): gather
    /// them per collection, run each collection's batch, and put each result
    /// back where its target was.
    pub(super) fn run_after_read_level(
        &self,
        docs: &mut [Document],
        fields: &[FieldDefinition],
        level: usize,
    ) {
        let mut batches: HashMap<String, (&'a CollectionDefinition, Vec<Document>)> =
            HashMap::new();

        self.visit_docs(docs, fields, level, &mut |def, value| {
            let obj = value.as_object().cloned().unwrap_or_default();

            batches
                .entry(def.slug.to_string())
                .or_insert_with(|| (def, Vec::new()))
                .1
                .push(embedded_to_document(obj));
        });

        let mut results: HashMap<String, vec::IntoIter<Value>> = batches
            .into_values()
            .map(|(def, batch)| (def.slug.to_string(), self.after_read_batch(def, batch)))
            .map(|(slug, values)| (slug, values.into_iter()))
            .collect();

        self.visit_docs(docs, fields, level, &mut |def, value| {
            *value = results
                .get_mut(def.slug.as_ref())
                .and_then(Iterator::next)
                .unwrap_or(Value::Null);
        });
    }

    /// Run `def`'s `after_read` hooks on `batch`, its targets of one level, in
    /// one call, and re-tag each result. The hooks see each target as a
    /// document (envelope keys lifted out), so they can neither read nor
    /// change the tag.
    fn after_read_batch(&self, def: &CollectionDefinition, batch: Vec<Document>) -> Vec<Value> {
        // Kept targets passed their collection's `before_read`, whose outcome
        // is memoized; a refusal here would mean nothing of it was kept.
        let Some(context) = self.read_context(def) else {
            return vec![Value::Null; batch.len()];
        };

        let reader = &self.reader;
        let ctx = AfterReadCtx {
            hooks: &def.hooks,
            fields: &def.fields,
            collection: &def.slug,
            operation: reader.operation,
            locale: reader.hook_locale,
            user: reader.user,
            ui_locale: reader.ui_locale,
            context,
        };

        reader
            .hooks
            .after_read_many(&ctx, batch)
            .iter()
            .map(|doc| document_to_json(doc, Some(def.slug.as_ref())))
            .collect()
    }

    /// Visit, in document order, every target at `level` in `docs`.
    fn visit_docs(
        &self,
        docs: &mut [Document],
        fields: &[FieldDefinition],
        level: usize,
        visitor: &mut LevelVisitor<'_, 'a>,
    ) {
        for doc in docs.iter_mut() {
            self.visit_level(&mut doc.fields, fields, LevelPos::new(level, 1), visitor);
        }
    }

    /// Visit the targets at `pos.target` below `root` (an object of the
    /// collection whose definitions are `fields`), whose own targets sit at
    /// `pos.current`. Follows exactly the embedding fields and targets the
    /// processing kept, in the same order.
    fn visit_level<R: JsonRoot>(
        &self,
        root: &mut R,
        fields: &[FieldDefinition],
        pos: LevelPos,
        visitor: &mut LevelVisitor<'_, 'a>,
    ) {
        let mut path: Vec<NestStep<'_>> = Vec::new();

        walk_nested_mut(root, fields, &mut path, &mut |field, level, _path| {
            if !is_embedding(field) {
                return VisitAction::Keep;
            }

            let Some(value) = level.root_get(&field.name) else {
                return VisitAction::Keep;
            };

            let mut value = value.clone();

            if !self.visit_value(&mut value, field, pos, visitor) {
                return VisitAction::Keep;
            }

            VisitAction::Replace(value)
        });
    }

    /// Visit the targets of one embedding field's value — a has-one target or
    /// the targets of a has-many / join array. Whether it held any.
    fn visit_value(
        &self,
        value: &mut Value,
        field: &FieldDefinition,
        pos: LevelPos,
        visitor: &mut LevelVisitor<'_, 'a>,
    ) -> bool {
        if value.as_object().is_some_and(is_embedded_ref) {
            self.visit_ref(value, field, pos, visitor);
            return true;
        }

        let Value::Array(items) = value else {
            return false;
        };

        let mut any = false;

        for item in items.iter_mut().filter(|item| value_is_embedded_ref(item)) {
            self.visit_ref(item, field, pos, visitor);
            any = true;
        }

        any
    }

    /// Visit one target: hand it to `visitor` at the target level, else walk
    /// its own targets one level deeper.
    fn visit_ref(
        &self,
        value: &mut Value,
        field: &FieldDefinition,
        pos: LevelPos,
        visitor: &mut LevelVisitor<'_, 'a>,
    ) {
        let Some(def) = value
            .as_object()
            .and_then(|obj| self.embedded_target(field, obj))
        else {
            return;
        };

        if pos.current == pos.target {
            visitor(def, value);
            return;
        }

        if let Some(obj) = value.as_object_mut() {
            self.visit_level::<Map<String, Value>>(obj, &def.fields, pos.deeper(), visitor);
        }
    }
}
