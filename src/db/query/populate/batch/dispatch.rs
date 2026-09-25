//! Batch relationship population across multiple documents.
//!
//! References are collected across the whole batch and fetched with one query
//! per target collection, but every stored reference is then populated along
//! its OWN ancestor path — the documents between it and the read's top level.
//! A document reached through two parents is expanded under each, and only a
//! reference back to a document on its own path stays an id: exactly the tree
//! the single-document path builds, so a list read and a read by id return the
//! same shape for the same document at every depth.

use std::mem;

use anyhow::Result;
use serde_json::Value;

use crate::core::{
    CollectionDefinition, Document, FieldType, cache::CacheBackend, field::flatten_array_sub_fields,
};
use crate::db::query::populate::{
    CachedDoc, PopulateContext, PopulateCtx, PopulateOpts, Singleflight, Visited, document_to_json,
    join::{document_join_fields, fetch_join_children},
    single::nested,
};

use super::refs::populate_reference_field;

/// One level of a batch populate: the collection its documents belong to, the
/// shared populate context at this level's depth, and the read's `select`.
struct Level<'a> {
    ctx: &'a PopulateContext<'a>,
    pctx: PopulateCtx<'a>,
    select: Option<&'a [String]>,
}

/// Batch-populate relationship fields across a slice of documents.
///
/// Collects all referenced IDs across all documents per field, batch-fetches them
/// with a single query per target collection, then distributes the results back.
pub(crate) fn populate_relationships_batch_cached(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
) -> Result<()> {
    // Fresh singleflight for this batch. The batch path already collapses
    // per-collection fetches, so per-id dedup matters mainly for nested
    // container recursion into single-doc paths.
    let singleflight = Singleflight::new();

    populate_relationships_batch_cached_with_singleflight(ctx, docs, opts, cache, &singleflight)
}

/// Variant of [`populate_relationships_batch_cached`] that accepts an
/// externally owned singleflight so concurrent populate trees across
/// requests can deduplicate cache-miss DB fetches for the same target.
///
/// Callers in the service layer pass the process-wide
/// [`SharedPopulateSingleflight`](crate::db::query::SharedPopulateSingleflight)
/// here. Internal callers keep using the fresh-per-call variant above.
///
/// # Errors
///
/// Returns a backend error if any relationship-target lookup fails.
pub fn populate_relationships_batch_cached_with_singleflight(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<CachedDoc>,
) -> Result<()> {
    let ancestors = vec![Visited::new(); docs.len()];

    populate_batch_with_paths(ctx, docs, opts, (cache, singleflight), &ancestors)
}

/// Core of the batch populate. `ancestors[i]` is the path above `docs[i]` —
/// every `(collection, id)` between it and the read's top level; each document
/// populates its references along that path plus itself.
///
/// A document already on its own path (a join child that is also one of its
/// ancestors) is embedded as it is, not expanded again — as the
/// single-document path does.
pub(super) fn populate_batch_with_paths(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    (cache, singleflight): (&dyn CacheBackend, &Singleflight<CachedDoc>),
    ancestors: &[Visited],
) -> Result<()> {
    if opts.depth <= 0 || docs.is_empty() {
        return Ok(());
    }

    let expand: Vec<usize> = (0..docs.len())
        .filter(|&i| !ancestors[i].contains(&visit_key(ctx.collection_slug, &docs[i])))
        .collect();

    let mut work: Vec<Document> = expand
        .iter()
        .map(|&i| mem::replace(&mut docs[i], Document::new(String::new())))
        .collect();

    let paths: Vec<Visited> = expand
        .iter()
        .zip(&work)
        .map(|(&i, doc)| own_path(&ancestors[i], ctx.collection_slug, doc))
        .collect();

    let level = Level {
        ctx,
        pctx: PopulateCtx {
            conn: ctx.conn,
            registry: ctx.registry,
            effective_depth: opts.depth,
            root_id: "",
            locale_ctx: opts.locale_ctx,
            published_only: opts.published_only,
            cache,
            singleflight,
            join_access: opts.join_access,
            user: opts.user,
        },
        select: opts.select,
    };

    let populated = populate_level(&level, &mut work, &paths);

    for (&i, doc) in expand.iter().zip(work) {
        docs[i] = doc;
    }

    populated
}

/// Populate one level: relationship/upload fields (through layout wrappers),
/// the ones inside groups/arrays/blocks, then the join fields.
fn populate_level(level: &Level<'_>, docs: &mut [Document], paths: &[Visited]) -> Result<()> {
    populate_flat_relationships(level, docs, paths)?;
    populate_nested_containers(level, docs, paths)?;
    populate_join_fields(level, docs, paths)
}

/// The cycle-guard key of a document of `collection`.
fn visit_key(collection: &str, doc: &Document) -> (String, String) {
    (collection.to_string(), doc.id.to_string())
}

/// `doc`'s own path: its ancestors plus itself.
fn own_path(ancestors: &Visited, collection: &str, doc: &Document) -> Visited {
    let mut path = ancestors.clone();
    path.insert(visit_key(collection, doc));
    path
}

/// Whether the read's `select` keeps `name`.
fn selected(select: Option<&[String]>, name: &str) -> bool {
    select.is_none_or(|sel| sel.iter().any(|s| s == name))
}

/// Populate non-join relationship/upload fields (flattened through transparent containers).
fn populate_flat_relationships(
    level: &Level<'_>,
    docs: &mut [Document],
    paths: &[Visited],
) -> Result<()> {
    let registry = level.pctx.registry;

    for field in flatten_array_sub_fields(level.ctx.fields) {
        if !matches!(
            field.field_type,
            FieldType::Relationship | FieldType::Upload
        ) || !selected(level.select, &field.name)
        {
            continue;
        }

        let Some(rel) = &field.relationship else {
            continue;
        };

        let effective_depth = rel.cap_depth(level.pctx.effective_depth);

        if effective_depth <= 0
            || (!rel.is_polymorphic() && registry.get_collection(&rel.collection).is_none())
        {
            continue;
        }

        let pctx = PopulateCtx {
            effective_depth,
            ..level.pctx
        };

        populate_reference_field(&pctx, docs, paths, (&field.name, rel))?;
    }

    Ok(())
}

/// Populate relationship fields inside nested containers (Groups/Blocks/Arrays),
/// each document along its own path.
fn populate_nested_containers(
    level: &Level<'_>,
    docs: &mut [Document],
    paths: &[Visited],
) -> Result<()> {
    for (doc, path) in docs.iter_mut().zip(paths) {
        let mut path = path.clone();
        // This doc's id anchors any reverse-join nested in its containers.
        let root_id = doc.id.to_string();
        let nested_pctx = PopulateCtx {
            root_id: &root_id,
            ..level.pctx
        };

        nested::populate_containers_in_doc(&nested_pctx, doc, level.ctx.fields, &mut path)?;
    }

    Ok(())
}

/// Populate the batch's document-level join fields: one grouped lookup per
/// join for every parent, each child then populated along its parent's path.
fn populate_join_fields(level: &Level<'_>, docs: &mut [Document], paths: &[Visited]) -> Result<()> {
    for field in document_join_fields(level.ctx.fields, level.select) {
        let Some(join) = &field.join else {
            continue;
        };

        let Some(target_def) = level.pctx.registry.get_collection(&join.collection) else {
            continue;
        };

        let parent_ids = docs.iter().map(|doc| doc.id.to_string()).collect();
        let buckets = fetch_join_children(&level.pctx, join, target_def, parent_ids)?;

        let mut children = JoinChildren::default();

        for (i, doc) in docs.iter().enumerate() {
            for child in buckets.get(doc.id.as_ref()).into_iter().flatten() {
                children.push(i, child.clone(), &paths[i]);
            }
        }

        descend(
            &level.pctx,
            (&join.collection, target_def),
            &mut children.docs,
            &children.ancestors,
        )?;

        children.assign(docs, &field.name, &join.collection);
    }

    Ok(())
}

/// The children of one join across a batch, each with its parent's index and
/// path.
#[derive(Default)]
struct JoinChildren {
    owners: Vec<usize>,
    docs: Vec<Document>,
    ancestors: Vec<Visited>,
}

impl JoinChildren {
    fn push(&mut self, owner: usize, child: Document, path: &Visited) {
        self.owners.push(owner);
        self.docs.push(child);
        self.ancestors.push(path.clone());
    }

    /// Write each parent's children (every parent gets an array, empty when
    /// it has none) under `field`, tagged with the target `collection`.
    fn assign(self, docs: &mut [Document], field: &str, collection: &str) {
        let mut lists: Vec<Vec<Value>> = vec![Vec::new(); docs.len()];

        for (owner, child) in self.owners.into_iter().zip(self.docs) {
            lists[owner].push(document_to_json(&child, Some(collection)));
        }

        for (doc, list) in docs.iter_mut().zip(lists) {
            doc.fields.insert(field.to_string(), Value::Array(list));
        }
    }
}

/// Populate fetched targets of `collection` one level deeper, each along its
/// own path (`ancestors[i]` for `docs[i]`), when the level's depth allows.
pub(super) fn descend(
    pctx: &PopulateCtx<'_>,
    (collection, def): (&str, &CollectionDefinition),
    docs: &mut [Document],
    ancestors: &[Visited],
) -> Result<()> {
    if pctx.effective_depth - 1 <= 0 {
        return Ok(());
    }

    // A fresh dedup table for the child level: an ancestor's own fetch may
    // still be in flight, and re-entering its table could wait on a key this
    // very call stack owns.
    let child_singleflight = Singleflight::new();

    populate_batch_with_paths(
        &PopulateContext {
            conn: pctx.conn,
            registry: pctx.registry,
            collection_slug: collection,
            fields: &def.fields,
        },
        docs,
        &PopulateOpts {
            depth: pctx.effective_depth - 1,
            select: None,
            locale_ctx: pctx.locale_ctx,
            published_only: pctx.published_only,
            join_access: pctx.join_access,
            user: pctx.user,
        },
        (pctx.cache, &child_singleflight),
        ancestors,
    )
}
