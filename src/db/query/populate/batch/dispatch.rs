//! Batch relationship population dispatch across multiple documents.

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

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

/// Populate reverse-lookup join fields across all `docs` in a batch.
///
/// For each join field, collects every parent doc's id, issues a single
/// `find(on_field IN (ids…))`, buckets results by `on_field`, and emits a
/// per-parent array. This replaces the previous per-parent-doc query pattern
/// (N+1 on the number of parents) — critical for `find_deep` throughput.
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

    if !has_join_fields || opts.depth <= 0 || docs.is_empty() {
        return Ok(());
    }

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

        // Build a shared filter clause for the field — access check runs once
        // per field, not once per parent doc. Denied → every parent gets [];
        // Constrained → extra filters merge into the batched query.
        let mut shared_filters: Vec<FilterClause> = Vec::new();
        let mut denied_all = false;

        if let Some(check) = opts.join_access {
            match check.check(target_def.access.read.as_deref(), opts.user)? {
                AccessResult::Denied => denied_all = true,
                AccessResult::Constrained(extra) => shared_filters.extend(extra),
                AccessResult::Allowed => {}
            }
        }

        if denied_all {
            for doc in docs.iter_mut() {
                doc.fields
                    .insert(field.name.clone(), Value::Array(Vec::new()));
            }
            continue;
        }

        // Collect unique parent ids. Order-preserving so output is deterministic.
        let mut parent_ids: Vec<String> = Vec::with_capacity(docs.len());
        let mut seen_ids: HashSet<String> = HashSet::new();
        for doc in docs.iter() {
            let id = doc.id.to_string();
            if seen_ids.insert(id.clone()) {
                parent_ids.push(id);
            }
        }

        let mut fq = FindQuery::new();
        fq.filters = shared_filters;
        fq.filters.push(FilterClause::Single(Filter {
            field: jc.on.clone(),
            op: FilterOp::In(parent_ids),
        }));

        let matched_docs =
            match read::find(ctx.conn, &jc.collection, &target_def, &fq, opts.locale_ctx) {
                Ok(docs) => docs,
                Err(_) => {
                    for doc in docs.iter_mut() {
                        doc.fields
                            .insert(field.name.clone(), Value::Array(Vec::new()));
                    }
                    continue;
                }
            };

        // Hydrate + populate each matched doc once (not per-parent). Nested
        // populates recurse at depth-1.
        let mut prepared: Vec<Document> = Vec::with_capacity(matched_docs.len());
        let mut nested_visited = visited.clone();
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
                &mut nested_visited,
                &PopulateOpts {
                    depth: opts.depth - 1,
                    select: None,
                    locale_ctx: opts.locale_ctx,
                    join_access: opts.join_access,
                    user: opts.user,
                },
                cache,
            )?;

            prepared.push(matched_doc);
        }

        // Bucket matched docs by their `on` field value so we can emit one
        // array per parent. A single matched doc only belongs to one parent
        // (the `on` column is a scalar foreign-key field).
        let mut buckets: HashMap<String, Vec<Value>> = HashMap::new();
        for matched_doc in &prepared {
            let key = match matched_doc.fields.get(&jc.on) {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string().trim_matches('"').to_string(),
                None => continue,
            };
            buckets
                .entry(key)
                .or_default()
                .push(document_to_json(matched_doc, &jc.collection));
        }

        for doc in docs.iter_mut() {
            let arr = buckets.remove(&doc.id.to_string()).unwrap_or_default();
            doc.fields.insert(field.name.clone(), Value::Array(arr));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::test_helpers::{
        make_authors_def_with_join, make_posts_def_for_join, setup_join_db,
    };
    use super::*;
    use crate::core::Registry;
    use crate::core::cache::NoneCache;

    /// Regression for the join-field N+1: batch populate across N parent docs
    /// must produce correct per-parent buckets. Before this change, the code
    /// issued one `find()` per parent; correctness was preserved but query
    /// count scaled with N. After the fix: one `IN (…)` query per field,
    /// results bucketed by the `on_field` value.
    #[test]
    fn batch_join_field_buckets_per_parent() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();

        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        // Two authors as parents: a1 (has posts p1, p2), a2 (has post p3).
        let mut docs = vec![
            {
                let mut d = Document::new("a1".to_string());
                d.fields.insert("name".to_string(), json!("Alice"));
                d
            },
            {
                let mut d = Document::new("a2".to_string());
                d.fields.insert("name".to_string(), json!("Bob"));
                d
            },
        ];

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        // a1 sees exactly its two posts.
        let a1_posts = docs[0]
            .fields
            .get("posts")
            .and_then(|v| v.as_array())
            .expect("a1 should get a posts array");
        assert_eq!(a1_posts.len(), 2, "a1 has 2 posts");
        let a1_titles: Vec<&str> = a1_posts
            .iter()
            .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
            .collect();
        assert!(a1_titles.contains(&"First Post"));
        assert!(a1_titles.contains(&"Second Post"));

        // a2 sees only its own post — no leakage from a1.
        let a2_posts = docs[1]
            .fields
            .get("posts")
            .and_then(|v| v.as_array())
            .expect("a2 should get a posts array");
        assert_eq!(a2_posts.len(), 1, "a2 has 1 post");
        assert_eq!(
            a2_posts[0].get("title").and_then(|t| t.as_str()),
            Some("Other Post")
        );
    }

    /// Batch path: an author with no matching posts must get an empty array,
    /// not a missing field. Before the batch rewrite this worked by accident
    /// because each parent ran its own query; after the rewrite the bucket
    /// lookup must still emit `[]` for no-match cases.
    #[test]
    fn batch_join_field_empty_bucket_for_parent_with_no_matches() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();

        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        // a99 is a parent with no matching posts — the setup_join_db fixture
        // has no posts with author='a99'.
        let mut docs = vec![{
            let mut d = Document::new("a99".to_string());
            d.fields.insert("name".to_string(), json!("Nobody"));
            d
        }];

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let posts = docs[0]
            .fields
            .get("posts")
            .and_then(|v| v.as_array())
            .expect("posts must still be present as an empty array");
        assert!(
            posts.is_empty(),
            "no-match parent must render as empty array, not missing"
        );
    }

    /// SEC-G guardrail preserved across the batch: Denied target access must
    /// leave every parent with an empty array, not an unfiltered fetch.
    #[test]
    fn batch_join_field_denies_for_all_parents_when_target_read_denied() {
        use crate::db::AccessResult;
        use crate::db::query::populate::JoinAccessCheck;
        use anyhow::Result as AnyResult;

        struct DenyAll;
        impl JoinAccessCheck for DenyAll {
            fn check(&self, _: Option<&str>, _: Option<&Document>) -> AnyResult<AccessResult> {
                Ok(AccessResult::Denied)
            }
        }

        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();

        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut docs = vec![
            {
                let mut d = Document::new("a1".to_string());
                d.fields.insert("name".to_string(), json!("Alice"));
                d
            },
            {
                let mut d = Document::new("a2".to_string());
                d.fields.insert("name".to_string(), json!("Bob"));
                d
            },
        ];

        let deny = DenyAll;
        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
                join_access: Some(&deny),
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        for (i, doc) in docs.iter().enumerate() {
            let arr = doc.fields.get("posts").and_then(|v| v.as_array()).unwrap();
            assert!(
                arr.is_empty(),
                "parent {i} must have empty posts under Denied"
            );
        }
    }
}
