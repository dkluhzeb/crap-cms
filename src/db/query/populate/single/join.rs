//! Join field (virtual reverse lookup) population.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

use tracing::warn;

use super::populate_relationships_cached;
use crate::core::cache::CacheBackend;
use crate::db::query::populate::{PopulateContext, PopulateOpts, document_to_json};
use crate::{
    core::{Document, FieldType, upload},
    db::{
        AccessResult, Filter, FilterClause, FilterOp, FindQuery,
        query::{hydrate_document, read::find},
    },
};

/// Populate join fields (virtual reverse lookups).
pub(super) fn populate_join_fields(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
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

        let populated = populate_join_docs(ctx, doc, jc, &target_def, visited, opts, cache)?;
        doc.fields
            .insert(field.name.clone(), Value::Array(populated));
    }

    Ok(())
}

/// Find, hydrate, and recursively populate matching documents for a join field.
fn populate_join_docs(
    ctx: &PopulateContext<'_>,
    doc: &Document,
    jc: &crate::core::field::JoinConfig,
    target_def: &crate::core::CollectionDefinition,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
) -> Result<Vec<Value>> {
    let mut fq = FindQuery::new();

    fq.filters = vec![FilterClause::Single(Filter {
        field: jc.on.clone(),
        op: FilterOp::Equals(doc.id.to_string()),
    })];

    // Target-collection access check (SEC-G). When hooks are wired in by the
    // service layer, honor the target's `access.read`. Denied => empty array.
    // Constrained => merge into fq.filters. Allowed => proceed as-is.
    if let Some(check) = opts.join_access {
        match check.check(target_def.access.read.as_deref(), opts.user)? {
            AccessResult::Denied => return Ok(Vec::new()),
            AccessResult::Constrained(extra) => fq.filters.extend(extra),
            AccessResult::Allowed => {}
        }
    }

    let matched_docs = match find(ctx.conn, &jc.collection, target_def, &fq, opts.locale_ctx) {
        Ok(docs) => docs,
        Err(e) => {
            warn!("join populate find error for {}: {e:#}", jc.collection);
            return Ok(Vec::new());
        }
    };

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
                def: target_def,
            },
            &mut matched_doc,
            visited,
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

    Ok(populated)
}

#[cfg(test)]
mod tests {
    use anyhow::Result as AnyResult;
    use serde_json::json;

    use super::super::super::test_helpers::*;
    use super::super::super::{JoinAccessCheck, PopulateContext, PopulateOpts};
    use super::populate_relationships_cached;
    use crate::core::cache::NoneCache;
    use crate::core::{Document, Registry};
    use crate::db::{AccessResult, Filter, FilterClause, FilterOp};
    use std::collections::HashSet;

    /// Fixture check: Denied for every call.
    struct DenyAll;
    impl JoinAccessCheck for DenyAll {
        fn check(&self, _: Option<&str>, _: Option<&Document>) -> AnyResult<AccessResult> {
            Ok(AccessResult::Denied)
        }
    }

    /// Fixture check: Allowed for every call.
    struct AllowAll;
    impl JoinAccessCheck for AllowAll {
        fn check(&self, _: Option<&str>, _: Option<&Document>) -> AnyResult<AccessResult> {
            Ok(AccessResult::Allowed)
        }
    }

    /// Fixture check: constrained with a filter that won't match any post
    /// (forces empty result after the filter merge, without needing _status).
    struct ConstrainToTitle(&'static str);
    impl JoinAccessCheck for ConstrainToTitle {
        fn check(&self, _: Option<&str>, _: Option<&Document>) -> AnyResult<AccessResult> {
            Ok(AccessResult::Constrained(vec![FilterClause::Single(
                Filter {
                    field: "title".to_string(),
                    op: FilterOp::Equals(self.0.to_string()),
                },
            )]))
        }
    }

    #[test]
    fn join_field_populates_reverse_docs() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
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

        let posts = doc
            .fields
            .get("posts")
            .expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert_eq!(arr.len(), 2, "Alice has 2 posts");

        let titles: Vec<&str> = arr
            .iter()
            .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
            .collect();
        assert!(titles.contains(&"First Post"));
        assert!(titles.contains(&"Second Post"));
    }

    #[test]
    fn join_field_depth_zero_noop() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 0,
                select: None,
                locale_ctx: None,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        // At depth=0, join field should not be populated
        assert!(
            !doc.fields.contains_key("posts"),
            "depth=0 should not add join field"
        );
    }

    #[test]
    fn join_field_no_matching_docs() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        // Author with no posts
        let mut doc = Document::new("a99".to_string());
        doc.fields.insert("name".to_string(), json!("Nobody"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
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

        let posts = doc
            .fields
            .get("posts")
            .expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert!(
            arr.is_empty(),
            "no matching posts should produce empty array"
        );
    }

    #[test]
    fn join_field_select_filters() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        // Select only "name", not "posts"
        let select = vec!["name".to_string()];
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: Some(&select),
                locale_ctx: None,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        // Join field should be skipped because it's not in select
        assert!(
            !doc.fields.contains_key("posts"),
            "join field not in select should be skipped"
        );
    }

    /// SEC-G regression: when the target collection's read access hook denies,
    /// the join field must produce an empty array — the target docs are not
    /// exfiltrated through the reverse-lookup.
    #[test]
    fn join_field_denies_when_target_read_access_denied() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        let deny = DenyAll;
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
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

        let posts = doc
            .fields
            .get("posts")
            .expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert!(
            arr.is_empty(),
            "denied target access must produce an empty join array"
        );
    }

    /// SEC-G regression: Constrained access merges filters into the find, so
    /// only docs matching the constraint are returned.
    #[test]
    fn join_field_constrained_by_target_read_access() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        // Only "First Post" passes the constraint.
        let constrained = ConstrainToTitle("First Post");
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
                join_access: Some(&constrained),
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let posts = doc
            .fields
            .get("posts")
            .expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert_eq!(arr.len(), 1, "constraint limits to one post");
        assert_eq!(
            arr[0].get("title").and_then(|t| t.as_str()),
            Some("First Post")
        );
    }

    /// SEC-G regression: Allowed access proceeds as today; legacy callers
    /// without a hook also behave unchanged (covered by
    /// `join_field_populates_reverse_docs` above).
    #[test]
    fn join_field_allowed_by_target_read_access() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        let allow = AllowAll;
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
                join_access: Some(&allow),
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let arr = doc
            .fields
            .get("posts")
            .and_then(|v| v.as_array())
            .expect("posts should be an array");
        assert_eq!(arr.len(), 2, "Allowed access returns all target docs");
    }

    /// Legacy path (no hooks wired) still works for internal callers.
    #[test]
    fn join_field_without_hooks_behaves_as_before() {
        // This is covered by `join_field_populates_reverse_docs` which uses
        // `join_access: None` — keep the explicit name for audit traceability.
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                def: &authors_def,
            },
            &mut doc,
            &mut visited,
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

        let arr = doc
            .fields
            .get("posts")
            .and_then(|v| v.as_array())
            .expect("posts should be an array");
        assert_eq!(arr.len(), 2);
    }
}
