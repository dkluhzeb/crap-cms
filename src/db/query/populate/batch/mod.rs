//! Batch relationship population across multiple documents.

mod nonpoly;
mod poly;
#[cfg(test)]
mod tests;

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

use crate::db::query::populate::{
    PopulateCache, PopulateContext, PopulateCtx, PopulateOpts, document_to_json,
};
use crate::{
    core::{Document, FieldType, field::flatten_array_sub_fields, upload},
    db::{
        Filter, FilterClause, FilterOp, FindQuery,
        query::{hydrate_document, read},
    },
};

use super::single::{nested, populate_relationships_cached};

/// Batch-populate relationship fields across a slice of documents.
///
/// Instead of calling `populate_relationships` per-document (N*M individual queries),
/// this collects all referenced IDs across all documents per field, batch-fetches them
/// with a single `find_by_ids` query per target collection, then distributes the results
/// back. Uses a shared `cache` to avoid redundant fetches within the same request.
///
/// This is the hot path for `Find` with `depth >= 1` — it turns O(N*M) queries into
/// O(M) queries where M is the number of relationship fields.
pub fn populate_relationships_batch_cached(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
    cache: &PopulateCache,
) -> Result<()> {
    let conn = ctx.conn;
    let registry = ctx.registry;
    let collection_slug = ctx.collection_slug;
    let def = ctx.def;
    let depth = opts.depth;
    let select = opts.select;
    let locale_ctx = opts.locale_ctx;

    if depth <= 0 || docs.is_empty() {
        return Ok(());
    }

    // Shared visited set across all documents for cross-document dedup
    let mut visited: HashSet<(String, String)> = HashSet::new();
    // Mark all parent documents as visited to prevent circular population
    for doc in docs.iter() {
        visited.insert((collection_slug.to_string(), doc.id.to_string()));
    }

    // -- Non-join relationship/upload fields (flattened through transparent containers) --
    for field in flatten_array_sub_fields(&def.fields) {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }
        if let Some(sel) = select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }
        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        let effective_depth = match rel.max_depth {
            Some(max) if max < depth => max,
            _ => depth,
        };

        if effective_depth <= 0 {
            continue;
        }

        let pctx = PopulateCtx {
            conn,
            registry,
            effective_depth,
            locale_ctx,
            cache,
        };

        if rel.is_polymorphic() {
            if rel.has_many {
                poly::batch_poly_has_many(&pctx, docs, &field.name, &visited)?;
            } else {
                poly::batch_poly_has_one(&pctx, docs, &field.name, &visited)?;
            }
        } else {
            let rel_def = match registry.get_collection(&rel.collection) {
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
                    &visited,
                )?;
            } else {
                nonpoly::batch_nonpoly_has_one(
                    &pctx,
                    docs,
                    &field.name,
                    &rel.collection,
                    &rel_def,
                    &visited,
                )?;
            }
        }
    }

    // -- Nested containers: populate relationship/upload fields inside Groups/Blocks/Arrays --
    for doc in docs.iter_mut() {
        let mut doc_visited = visited.clone();
        let nested_pctx = PopulateCtx {
            conn,
            registry,
            effective_depth: depth,
            locale_ctx,
            cache,
        };
        nested::populate_containers_in_doc(&nested_pctx, doc, &def.fields, &mut doc_visited)?;
    }

    // -- Join fields: fall through to per-doc (reverse lookups can't batch easily) --
    let has_join_fields = def.fields.iter().any(|f| {
        f.field_type == FieldType::Join
            && f.join.is_some()
            && select.is_none_or(|sel| sel.iter().any(|s| s == &f.name))
    });

    if has_join_fields && depth > 0 {
        for doc in docs.iter_mut() {
            let mut doc_visited = visited.clone();
            for field in &def.fields {
                if field.field_type != FieldType::Join {
                    continue;
                }
                if let Some(sel) = select
                    && !sel.iter().any(|s| s == &field.name)
                {
                    continue;
                }
                let jc = match &field.join {
                    Some(jc) => jc,
                    None => continue,
                };
                let target_def = match registry.get_collection(&jc.collection) {
                    Some(d) => d.clone(),
                    None => continue,
                };
                let mut fq = FindQuery::new();
                fq.filters = vec![FilterClause::Single(Filter {
                    field: jc.on.clone(),
                    op: FilterOp::Equals(doc.id.to_string()),
                })];
                let fq = fq;

                if let Ok(matched_docs) =
                    read::find(conn, &jc.collection, &target_def, &fq, locale_ctx)
                {
                    let mut populated = Vec::new();
                    for mut matched_doc in matched_docs {
                        hydrate_document(
                            conn,
                            &jc.collection,
                            &target_def.fields,
                            &mut matched_doc,
                            None,
                            locale_ctx,
                        )?;

                        if let Some(ref uc) = target_def.upload
                            && uc.enabled
                        {
                            upload::assemble_sizes_object(&mut matched_doc, uc);
                        }
                        populate_relationships_cached(
                            &PopulateContext {
                                conn,
                                registry,
                                collection_slug: &jc.collection,
                                def: &target_def,
                            },
                            &mut matched_doc,
                            &mut doc_visited,
                            &PopulateOpts {
                                depth: depth - 1,
                                select: None,
                                locale_ctx,
                            },
                            cache,
                        )?;
                        populated.push(document_to_json(&matched_doc, &jc.collection));
                    }
                    doc.fields
                        .insert(field.name.clone(), Value::Array(populated));
                }
            }
        }
    }

    Ok(())
}
