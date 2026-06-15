//! Join field (virtual reverse lookup) population.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

use tracing::warn;

use super::populate_relationships_cached;
use crate::core::cache::CacheBackend;
use crate::db::query::populate::helpers::{resolve_views, target_row_visible};
use crate::db::query::populate::{PopulateContext, PopulateOpts, document_to_json};
use crate::{
    core::{Document, FieldType, upload},
    db::{
        Filter, FilterClause, FilterOp, FindQuery,
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

        let Some(jc) = &field.join else {
            continue;
        };

        let Some(target_def) = ctx.registry.get_collection(&jc.collection).cloned() else {
            continue;
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
    jc: &crate::core::JoinConfig,
    target_def: &crate::core::CollectionDefinition,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
) -> Result<Vec<Value>> {
    let filters = vec![FilterClause::Single(Filter {
        field: jc.on.clone(),
        op: FilterOp::Equals(doc.id.to_string()),
    })];

    // Resolve the target collection's view access (read + draft). The matched
    // children are filtered per row below by the same independent-views model
    // used for relationships — a published child needs `read`, a draft child
    // needs drafts requested (`!published_only`) AND the target's `draft`
    // access. (`find` already excludes trashed rows.)
    let views = resolve_views(opts.join_access, opts.user, &target_def.slug, target_def)?;

    let fq = FindQuery::builder().filters(filters).build();

    let matched_docs = match find(ctx.conn, &jc.collection, target_def, &fq, opts.locale_ctx) {
        Ok(docs) => docs,
        Err(e) => {
            warn!("join populate find error for {}: {e:#}", jc.collection);
            return Ok(Vec::new());
        }
    };

    let mut populated = Vec::new();

    for mut matched_doc in matched_docs {
        // Per-row visibility: drop children the viewer may not see (a draft
        // child without target draft access, or a row failing a view's
        // row-constraints), matched against the RAW fields before hydration.
        if !target_row_visible(&views, &matched_doc, opts.published_only, target_def) {
            continue;
        }

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
                published_only: opts.published_only,
                join_access: opts.join_access,
                user: opts.user,
            },
            cache,
        )?;

        populated.push(document_to_json(&matched_doc, &jc.collection));
    }

    Ok(populated)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result as AnyResult;
    use serde_json::json;

    use super::populate_relationships_cached;
    use crate::core::cache::NoneCache;
    use crate::core::{Document, HookRef, Registry};
    use crate::db::query::populate::test_helpers::*;
    use crate::db::query::populate::{JoinAccessCheck, PopulateContext, PopulateOpts};
    use crate::db::{AccessResult, Filter, FilterClause, FilterOp};
    use std::collections::HashSet;

    /// Fixture check: Denied for every call.
    struct DenyAll;
    impl JoinAccessCheck for DenyAll {
        fn check(
            &self,
            _: Option<&HookRef>,
            _: Option<&Document>,
            _: &str,
        ) -> AnyResult<AccessResult> {
            Ok(AccessResult::Denied)
        }
    }

    /// Fixture check: Allowed for every call.
    struct AllowAll;
    impl JoinAccessCheck for AllowAll {
        fn check(
            &self,
            _: Option<&HookRef>,
            _: Option<&Document>,
            _: &str,
        ) -> AnyResult<AccessResult> {
            Ok(AccessResult::Allowed)
        }
    }

    /// Fixture check: constrained with a filter that won't match any post
    /// (forces empty result after the filter merge, without needing _status).
    struct ConstrainToTitle(&'static str);
    impl JoinAccessCheck for ConstrainToTitle {
        fn check(
            &self,
            _: Option<&HookRef>,
            _: Option<&Document>,
            _: &str,
        ) -> AnyResult<AccessResult> {
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
                published_only: false,
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

    /// Regression (P6.5 — the join draft leak): a DRAFT join child is hidden
    /// unless drafts are *requested* (`!published_only`) AND the viewer holds the
    /// target's `draft` access. Joins previously applied no status filter at all,
    /// so any reader with `read` saw every collection's draft rows through a join.
    #[test]
    fn join_draft_child_gated_by_target_draft_access() {
        use crate::core::VersionsConfig;
        use rusqlite::Connection;

        // read → Allowed; draft (`draft_fn`) → configurable.
        struct DraftGate(bool);
        impl JoinAccessCheck for DraftGate {
            fn check(
                &self,
                access: Option<&HookRef>,
                _: Option<&Document>,
                _: &str,
            ) -> AnyResult<AccessResult> {
                let is_draft = access.map(HookRef::reference) == Some("draft_fn");
                Ok(if is_draft && !self.0 {
                    AccessResult::Denied
                } else {
                    AccessResult::Allowed
                })
            }
        }

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts (
                id TEXT PRIMARY KEY, title TEXT, author TEXT,
                _status TEXT NOT NULL DEFAULT 'published', created_at TEXT, updated_at TEXT
             );
             INSERT INTO authors VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
             INSERT INTO posts (id, title, author, _status, created_at, updated_at)
                VALUES ('p1', 'Published Post', 'a1', 'published', '2024-01-01', '2024-01-01');
             INSERT INTO posts (id, title, author, _status, created_at, updated_at)
                VALUES ('p2', 'Draft Post', 'a1', 'draft', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let authors_def = make_authors_def_with_join();
        let mut posts_def = make_posts_def_for_join();
        posts_def.versions = Some(VersionsConfig::new(true, 0)); // has_drafts()
        posts_def.access.read = Some(HookRef::new("read_fn"));
        posts_def.access.draft = Some(HookRef::new("draft_fn"));
        assert!(posts_def.has_drafts());

        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let titles = |published_only: bool, draft_allowed: bool| -> Vec<String> {
            let mut doc = Document::new("a1".to_string());
            doc.fields.insert("name".to_string(), json!("Alice"));
            let gate = DraftGate(draft_allowed);
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
                    published_only,
                    join_access: Some(&gate),
                    user: None,
                },
                &NoneCache,
            )
            .unwrap();
            doc.fields
                .get("posts")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        };

        // Published-only read: the draft post is never embedded.
        assert_eq!(
            titles(true, true),
            vec!["Published Post"],
            "published-only join read must exclude draft children"
        );

        // Drafts REQUESTED but target draft access DENIED: still excluded — this
        // is the leak the fix closes.
        assert_eq!(
            titles(false, false),
            vec!["Published Post"],
            "a draft join child must be hidden without the target's draft access"
        );

        // Drafts requested AND target draft access granted: both visible.
        let mut both = titles(false, true);
        both.sort();
        assert_eq!(both, vec!["Draft Post", "Published Post"]);
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
                published_only: false,
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
                published_only: false,
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
                published_only: false,
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
                published_only: false,
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
                published_only: false,
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
                published_only: false,
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
                published_only: false,
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
