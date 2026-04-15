//! Batch relationship population dispatch across multiple documents.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

use crate::core::cache::CacheBackend;
use crate::core::{Document, FieldType, field::flatten_array_sub_fields, upload};
use crate::db::query::populate::{
    PopulateContext, PopulateCtx, PopulateOpts, Singleflight, document_to_json,
};
use crate::db::{
    AccessResult, Filter, FilterClause, FilterOp, FindQuery,
    query::{hydrate_document, read},
};

use super::super::single::{nested, populate_relationships_cached};
use super::{nonpoly, poly};

/// Batch-populate relationship fields across a slice of documents.
///
/// Collects all referenced IDs across all documents per field, batch-fetches them
/// with a single query per target collection, then distributes the results back.
pub fn populate_relationships_batch_cached(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
) -> Result<()> {
    // Fresh singleflight for this batch. The batch path already collapses
    // per-collection fetches via `find_by_ids`, so per-id dedup matters mainly
    // for nested container recursion into single-doc paths.
    let singleflight = Singleflight::new();

    populate_relationships_batch_cached_inner(ctx, docs, opts, cache, &singleflight)
}

/// Variant of [`populate_relationships_batch_cached`] that accepts an
/// externally owned singleflight so concurrent populate trees across
/// requests can deduplicate cache-miss DB fetches for the same target.
///
/// Callers in the service layer pass the process-wide
/// [`SharedPopulateSingleflight`](crate::db::query::SharedPopulateSingleflight)
/// here. Internal callers keep using the fresh-per-call variant above.
pub fn populate_relationships_batch_cached_with_singleflight(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
) -> Result<()> {
    populate_relationships_batch_cached_inner(ctx, docs, opts, cache, singleflight)
}

fn populate_relationships_batch_cached_inner(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
) -> Result<()> {
    if opts.depth <= 0 || docs.is_empty() {
        return Ok(());
    }

    let mut visited: HashSet<(String, String)> = HashSet::new();

    for doc in docs.iter() {
        visited.insert((ctx.collection_slug.to_string(), doc.id.to_string()));
    }

    populate_flat_relationships(ctx, docs, opts, cache, singleflight, &visited)?;
    populate_nested_containers(ctx, docs, opts, cache, singleflight, &visited)?;
    populate_join_fields(ctx, docs, opts, cache, &visited)?;

    Ok(())
}

/// Populate non-join relationship/upload fields (flattened through transparent containers).
fn populate_flat_relationships(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
    visited: &HashSet<(String, String)>,
) -> Result<()> {
    for field in flatten_array_sub_fields(&ctx.def.fields) {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }

        if let Some(sel) = opts.select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }

        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        let effective_depth = match rel.max_depth {
            Some(max) if max < opts.depth => max,
            _ => opts.depth,
        };

        if effective_depth <= 0 {
            continue;
        }

        let pctx = PopulateCtx {
            conn: ctx.conn,
            registry: ctx.registry,
            effective_depth,
            locale_ctx: opts.locale_ctx,
            cache,
            singleflight,
        };

        if rel.is_polymorphic() {
            if rel.has_many {
                poly::batch_poly_has_many(&pctx, docs, &field.name, visited)?;
            } else {
                poly::batch_poly_has_one(&pctx, docs, &field.name, visited)?;
            }
        } else {
            let rel_def = match ctx.registry.get_collection(&rel.collection) {
                Some(d) => d.clone(),
                None => continue,
            };

            if rel.has_many {
                nonpoly::batch_nonpoly_has_many(
                    &pctx,
                    docs,
                    &field.name,
                    &rel.collection,
                    &rel_def,
                    visited,
                )?;
            } else {
                nonpoly::batch_nonpoly_has_one(
                    &pctx,
                    docs,
                    &field.name,
                    &rel.collection,
                    &rel_def,
                    visited,
                )?;
            }
        }
    }

    Ok(())
}

/// Populate relationship fields inside nested containers (Groups/Blocks/Arrays).
fn populate_nested_containers(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
    visited: &HashSet<(String, String)>,
) -> Result<()> {
    for doc in docs.iter_mut() {
        let mut doc_visited = visited.clone();
        let nested_pctx = PopulateCtx {
            conn: ctx.conn,
            registry: ctx.registry,
            effective_depth: opts.depth,
            locale_ctx: opts.locale_ctx,
            cache,
            singleflight,
        };

        nested::populate_containers_in_doc(&nested_pctx, doc, &ctx.def.fields, &mut doc_visited)?;
    }

    Ok(())
}

/// Populate reverse-lookup join fields (can't batch, falls through to per-doc).
fn populate_join_fields(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    visited: &HashSet<(String, String)>,
) -> Result<()> {
    let has_join_fields = ctx.def.fields.iter().any(|f| {
        f.field_type == FieldType::Join
            && f.join.is_some()
            && opts
                .select
                .is_none_or(|sel| sel.iter().any(|s| s == &f.name))
    });

    if !has_join_fields || opts.depth <= 0 {
        return Ok(());
    }

    for doc in docs.iter_mut() {
        let mut doc_visited = visited.clone();
        populate_join_fields_for_doc(ctx, doc, opts, cache, &mut doc_visited)?;
    }

    Ok(())
}

/// Populate join fields for a single document.
fn populate_join_fields_for_doc(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    doc_visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    for field in &ctx.def.fields {
        if field.field_type != FieldType::Join {
            continue;
        }

        if let Some(sel) = opts.select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }

        let jc = match &field.join {
            Some(jc) => jc,
            None => continue,
        };

        let target_def = match ctx.registry.get_collection(&jc.collection) {
            Some(d) => d.clone(),
            None => continue,
        };

        let mut fq = FindQuery::new();

        fq.filters = vec![FilterClause::Single(Filter {
            field: jc.on.clone(),
            op: FilterOp::Equals(doc.id.to_string()),
        })];

        // Target-collection access check (SEC-G). Denied => skip; Constrained => merge.
        if let Some(check) = opts.join_access {
            match check.check(target_def.access.read.as_deref(), opts.user)? {
                AccessResult::Denied => {
                    doc.fields
                        .insert(field.name.clone(), Value::Array(Vec::new()));
                    continue;
                }
                AccessResult::Constrained(extra) => fq.filters.extend(extra),
                AccessResult::Allowed => {}
            }
        }

        if let Ok(matched_docs) =
            read::find(ctx.conn, &jc.collection, &target_def, &fq, opts.locale_ctx)
        {
            let mut populated = Vec::new();

            for mut matched_doc in matched_docs {
                hydrate_document(
                    ctx.conn,
                    &jc.collection,
                    &target_def.fields,
                    &mut matched_doc,
                    None,
                    opts.locale_ctx,
                )?;

                if let Some(ref uc) = target_def.upload
                    && uc.enabled
                {
                    upload::assemble_sizes_object(&mut matched_doc, uc);
                }

                populate_relationships_cached(
                    &PopulateContext {
                        conn: ctx.conn,
                        registry: ctx.registry,
                        collection_slug: &jc.collection,
                        def: &target_def,
                    },
                    &mut matched_doc,
                    doc_visited,
                    &PopulateOpts {
                        depth: opts.depth - 1,
                        select: None,
                        locale_ctx: opts.locale_ctx,
                        join_access: opts.join_access,
                        user: opts.user,
                    },
                    cache,
                )?;

                populated.push(document_to_json(&matched_doc, &jc.collection));
            }

            doc.fields
                .insert(field.name.clone(), Value::Array(populated));
        }
    }

    Ok(())
}
