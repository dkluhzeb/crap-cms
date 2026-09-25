//! Join field (virtual reverse lookup) population for one document.

use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

use super::populate_relationships_cached;
use crate::core::{CollectionDefinition, Document, FieldDefinition, JoinConfig};
use crate::db::query::populate::{
    PopulateContext, PopulateCtx, PopulateOpts, document_to_json,
    join::{document_join_fields, fetch_join_children},
};

/// Populate `doc`'s document-level join fields — top level and in layout
/// wrappers (`fields` are its collection's); a join in a group is populated by
/// the container walker through [`populate_join_docs`].
///
/// # Errors
///
/// Propagates an access-hook or backend error.
pub(super) fn populate_join_fields(
    fields: &[FieldDefinition],
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    pctx: &PopulateCtx<'_>,
    select: Option<&[String]>,
) -> Result<()> {
    for field in document_join_fields(fields, select) {
        let Some(join) = &field.join else {
            continue;
        };

        let Some(target_def) = pctx.registry.get_collection(&join.collection) else {
            continue;
        };

        let children = populate_join_docs(pctx, &doc.id, join, target_def, visited)?;

        doc.fields
            .insert(field.name.clone(), Value::Array(children));
    }

    Ok(())
}

/// The children `join` lists for the document `doc_id` (see
/// [`fetch_join_children`]), each populated one level deeper along the current
/// path. The join is anchored to the document's id however deeply the field
/// sits in groups.
///
/// # Errors
///
/// Propagates an access-hook or backend error — a failed lookup never reads
/// as an empty join.
pub(super) fn populate_join_docs(
    pctx: &PopulateCtx<'_>,
    doc_id: &str,
    join: &JoinConfig,
    target_def: &CollectionDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<Vec<Value>> {
    let mut buckets = fetch_join_children(pctx, join, target_def, vec![doc_id.to_string()])?;
    let children = buckets.remove(doc_id).unwrap_or_default();

    let child_ctx = PopulateContext {
        conn: pctx.conn,
        registry: pctx.registry,
        collection_slug: &join.collection,
        fields: &target_def.fields,
    };
    let child_opts = PopulateOpts {
        depth: pctx.effective_depth - 1,
        select: None,
        locale_ctx: pctx.locale_ctx,
        published_only: pctx.published_only,
        join_access: pctx.join_access,
        user: pctx.user,
    };

    children
        .into_iter()
        .map(|mut child| {
            populate_relationships_cached(
                &child_ctx,
                &mut child,
                visited,
                &child_opts,
                pctx.cache,
            )?;

            Ok(document_to_json(&child, Some(&join.collection)))
        })
        .collect()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result as AnyResult;
    use serde_json::json;

    use super::populate_relationships_cached;
    use crate::core::cache::NoneCache;
    use crate::core::{Document, HookRef, Registry, VersionsConfig};
    use crate::db::query::populate::test_helpers::*;
    use crate::db::query::populate::{JoinAccessCheck, PopulateContext, PopulateOpts};
    use crate::db::{AccessResult, Filter, FilterClause, FilterOp};
    use rusqlite::Connection;
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
                fields: &authors_def.fields,
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

    /// A Join nested inside a Group is populated just like a top-level Join —
    /// the reverse lookup anchors on the document id regardless of nesting.
    #[test]
    fn nested_join_in_group_populates() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_nested_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), json!("Alice"));
        // The group must be present as an object for the walker to descend.
        doc.fields.insert("section".to_string(), json!({}));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "authors",
                fields: &authors_def.fields,
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

        let section = doc.fields.get("section").expect("section group present");
        let arr = section
            .get("posts")
            .and_then(|v| v.as_array())
            .expect("nested join field should be populated as an array");
        assert_eq!(arr.len(), 2, "Alice has 2 posts via the nested join");
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
        conn.execute_batch(&versions_table_sql("posts")).unwrap();
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
                    fields: &authors_def.fields,
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
                fields: &authors_def.fields,
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
                fields: &authors_def.fields,
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
                fields: &authors_def.fields,
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
                fields: &authors_def.fields,
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
                fields: &authors_def.fields,
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
                fields: &authors_def.fields,
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
                fields: &authors_def.fields,
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
