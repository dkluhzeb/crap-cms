//! Batch resolution of one relationship/upload field's stored references.
//!
//! Plan every reference of every document, fetch each target collection once,
//! then give each reference its OWN copy of its target, populated along the
//! referencing document's path (see the module docs of `dispatch`).

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::core::{CollectionDefinition, Document, RelationshipConfig};
use crate::db::query::populate::{
    PopulateCtx, Visited, document_to_json,
    helpers::{
        TargetCollection, cache_get_doc, cache_set_doc, fetch_targets, resolve_target_views,
        visible_targets,
    },
    locale_cache_key, parse_poly_ref, populate_cache_key,
};

use super::dispatch::descend;

/// One stored reference, planned before any fetch.
enum Planned {
    /// Left as stored: a reference back to a document on its own path, or a
    /// polymorphic reference naming no usable collection.
    Keep(Value),
    /// To resolve: `(collection, id)`.
    Fetch(String, String),
}

/// Where one stored reference lands once resolved.
enum Slot {
    /// Left as stored.
    Keep(Value),
    /// Missing, or hidden from this reader.
    Missing,
    /// The `usize`th resolved instance of the named collection.
    Target(String, usize),
}

/// Fetched targets, per collection, keyed by id.
type Fetched = HashMap<String, HashMap<String, Document>>;

/// Populate the relationship/upload field `name` (configured by `rel`) on every
/// document of the batch, `paths[i]` being `docs[i]`'s own path.
///
/// # Errors
///
/// Propagates an access-hook or backend error from a target fetch or the
/// recursive populate.
pub(super) fn populate_reference_field(
    pctx: &PopulateCtx<'_>,
    docs: &mut [Document],
    paths: &[Visited],
    (name, rel): (&str, &RelationshipConfig),
) -> Result<()> {
    let plans: Vec<Option<Vec<Planned>>> = docs
        .iter()
        .zip(paths)
        .map(|(doc, path)| plan_doc(pctx, doc.fields.get(name), path, rel))
        .collect();

    let fetched = fetch_planned(pctx, &plans)?;
    let mut instances = Instances::default();

    let slots: Vec<Option<Vec<Slot>>> = plans
        .into_iter()
        .zip(paths)
        .map(|(plan, path)| plan.map(|refs| instances.place_all(refs, &fetched, path)))
        .collect();

    let mut resolved = instances.populate(pctx)?;

    for (doc, slots) in docs.iter_mut().zip(slots) {
        if let Some(slots) = slots {
            write_field(doc, (name, rel.has_many), slots, &mut resolved);
        }
    }

    Ok(())
}

/// Plan one document's references in `value`. `None` when the field holds
/// nothing to populate (absent, empty, or not the stored shape).
fn plan_doc(
    pctx: &PopulateCtx<'_>,
    value: Option<&Value>,
    path: &Visited,
    rel: &RelationshipConfig,
) -> Option<Vec<Planned>> {
    let refs: Vec<&str> = match (rel.has_many, value) {
        (false, Some(Value::String(s))) if !s.is_empty() => vec![s.as_str()],
        (true, Some(Value::Array(items))) => items.iter().filter_map(Value::as_str).collect(),
        _ => return None,
    };

    Some(
        refs.into_iter()
            .map(|stored| plan_ref(pctx, stored, path, rel))
            .collect(),
    )
}

/// Plan one stored reference.
fn plan_ref(
    pctx: &PopulateCtx<'_>,
    stored: &str,
    path: &Visited,
    rel: &RelationshipConfig,
) -> Planned {
    let target = if rel.is_polymorphic() {
        parse_poly_ref(stored)
    } else {
        Some((rel.collection.to_string(), stored.to_string()))
    };

    let Some((collection, id)) = target else {
        return Planned::Keep(Value::String(stored.to_string()));
    };

    let key = (collection, id);

    if path.contains(&key) || pctx.registry.get_collection(&key.0).is_none() {
        return Planned::Keep(Value::String(stored.to_string()));
    }

    Planned::Fetch(key.0, key.1)
}

/// Fetch every planned target, one lookup per collection, keeping the ones
/// this reader may see (as it sees them).
fn fetch_planned(pctx: &PopulateCtx<'_>, plans: &[Option<Vec<Planned>>]) -> Result<Fetched> {
    let mut wanted: HashMap<&str, Vec<String>> = HashMap::new();

    for planned in plans.iter().flatten().flatten() {
        if let Planned::Fetch(collection, id) = planned {
            wanted.entry(collection).or_default().push(id.clone());
        }
    }

    let mut fetched = Fetched::new();

    for (collection, mut ids) in wanted {
        let Some(def) = pctx.registry.get_collection(collection) else {
            continue;
        };

        ids.sort();
        ids.dedup();

        let found = fetch_visible(pctx, (collection, def), &ids)?;
        fetched.insert(collection.to_string(), found);
    }

    Ok(fetched)
}

/// The targets of `ids` in `collection` this reader may see, keyed by id:
/// raw documents from the shared cache, else one query (caching each), then
/// the reader's visibility.
fn fetch_visible(
    pctx: &PopulateCtx<'_>,
    (collection, def): (&str, &CollectionDefinition),
    ids: &[String],
) -> Result<HashMap<String, Document>> {
    let views = resolve_target_views(pctx, collection, def)?;
    let raws = cached_raws(pctx, (collection, def), ids)?;

    let target = TargetCollection::builder(collection, def, &views).build();
    let visible = visible_targets(pctx, &target, raws)?;

    Ok(visible
        .into_iter()
        .map(|doc| (doc.id.to_string(), doc))
        .collect())
}

/// Raw target documents: the shared cache's hits (it holds raw,
/// user-independent content), then one DB fetch for the misses, caching each.
fn cached_raws(
    pctx: &PopulateCtx<'_>,
    (collection, def): (&str, &CollectionDefinition),
    ids: &[String],
) -> Result<Vec<Document>> {
    let locale_key = locale_cache_key(pctx.locale_ctx);
    let key = |id: &str| populate_cache_key(collection, id, locale_key.as_deref());

    let mut raws = Vec::new();
    let mut misses = Vec::new();

    for id in ids {
        match cache_get_doc(pctx.cache, &key(id)).ok().flatten() {
            Some(raw) => raws.push(raw),
            None => misses.push(id.clone()),
        }
    }

    if misses.is_empty() {
        return Ok(raws);
    }

    for raw in fetch_targets(pctx, collection, def, &misses)? {
        let _ = cache_set_doc(pctx.cache, &key(raw.id.as_ref()), &raw);
        raws.push(raw);
    }

    Ok(raws)
}

/// One target collection's resolved instances — one per reference — with the
/// path each is populated along.
#[derive(Default)]
struct Group {
    docs: Vec<Document>,
    ancestors: Vec<Visited>,
}

/// The instances of a field's targets, per collection.
#[derive(Default)]
struct Instances {
    by_collection: HashMap<String, Group>,
}

impl Instances {
    /// Place one document's planned references.
    fn place_all(&mut self, refs: Vec<Planned>, fetched: &Fetched, path: &Visited) -> Vec<Slot> {
        refs.into_iter()
            .map(|planned| self.place(planned, fetched, path))
            .collect()
    }

    /// Place one planned reference: its own copy of the fetched target, to be
    /// populated along `path` (the referencing document's own path).
    fn place(&mut self, planned: Planned, fetched: &Fetched, path: &Visited) -> Slot {
        let (collection, id) = match planned {
            Planned::Keep(value) => return Slot::Keep(value),
            Planned::Fetch(collection, id) => (collection, id),
        };

        let Some(target) = fetched.get(&collection).and_then(|found| found.get(&id)) else {
            return Slot::Missing;
        };

        let group = self.by_collection.entry(collection.clone()).or_default();
        group.docs.push(target.clone());
        group.ancestors.push(path.clone());

        Slot::Target(collection, group.docs.len() - 1)
    }

    /// Populate every instance one level deeper along its own path, and
    /// return each as its embedded JSON, per collection in placement order.
    fn populate(self, pctx: &PopulateCtx<'_>) -> Result<HashMap<String, Vec<Option<Value>>>> {
        let mut resolved = HashMap::new();

        for (collection, mut group) in self.by_collection {
            let Some(def) = pctx.registry.get_collection(&collection) else {
                continue;
            };

            descend(pctx, (&collection, def), &mut group.docs, &group.ancestors)?;

            let values = group
                .docs
                .iter()
                .map(|doc| Some(document_to_json(doc, Some(&collection))))
                .collect();

            resolved.insert(collection, values);
        }

        Ok(resolved)
    }
}

/// Write one document's resolved references back: a has-many keeps what
/// resolved (and the references left as stored), dropping missing ones; a
/// has-one becomes its target, `null` when missing, or stays as stored.
fn write_field(
    doc: &mut Document,
    (name, has_many): (&str, bool),
    slots: Vec<Slot>,
    resolved: &mut HashMap<String, Vec<Option<Value>>>,
) {
    let mut value_of = |slot: Slot| match slot {
        Slot::Keep(value) => Some(value),
        Slot::Missing => None,
        Slot::Target(collection, index) => resolved
            .get_mut(&collection)
            .and_then(|values| values.get_mut(index))
            .and_then(Option::take),
    };

    if has_many {
        let items: Vec<Value> = slots.into_iter().filter_map(&mut value_of).collect();
        doc.fields.insert(name.to_string(), Value::Array(items));
        return;
    }

    let Some(slot) = slots.into_iter().next() else {
        return;
    };

    if matches!(slot, Slot::Keep(_)) {
        return;
    }

    let value = value_of(slot).unwrap_or(Value::Null);
    doc.fields.insert(name.to_string(), value);
}
