//! Document information service — ref counts, back-references, missing relations.
//!
//! Thin service wrappers for consistency. All future surfaces should call these
//! instead of the query layer directly.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, Registry},
    db::{
        AccessResult, DbConnection, FilterClause,
        query::{self, BackReference, MissingRelation, filter_visible_ids},
    },
    hooks::AccessCheckInput,
    service::{ReadAccessCtx, ReadHooks, ServiceContext, resolve_visibility_filter},
};

use super::ServiceError;

/// Access-filtered back-references for a document.
///
/// The raw scan finds every referrer regardless of access; this report carries
/// only the referrers the viewer may see, plus a non-quantified flag for the
/// rest. The viewer never learns how many references they cannot access — only
/// that some exist. The delete-*block* decision is independent of this report:
/// it uses the raw `_ref_count`, which stays visibility-blind for system
/// integrity.
#[derive(Debug, Clone, Serialize)]
pub struct BackReferenceReport {
    /// Referrer groups the viewer is allowed to see. Each group's `count`
    /// reflects only visible documents.
    pub references: Vec<BackReference>,
    /// True when at least one referrer exists that the viewer cannot access —
    /// surfaced as a non-quantified "also referenced by documents you don't
    /// have access to" note. Never reveals the hidden count.
    pub has_inaccessible: bool,
}

/// Get the incoming reference count for a document.
///
/// Returns 0 if the `_ref_count` column is NULL or the document doesn't exist.
pub fn get_ref_count(ctx: &ServiceContext, id: &str) -> Result<i64, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let count = query::ref_count::get_ref_count(conn.as_ref(), ctx.slug, id)?.unwrap_or(0);
    Ok(count)
}

/// The resolved visibility of one owner collection/global for the viewer.
enum Visibility {
    /// Nothing in this owner is visible — drop every referrer it holds.
    Hidden,
    /// Every row is visible — keep referrers unfiltered.
    AllVisible,
    /// Only rows matching these filters are visible.
    Filtered(Vec<FilterClause>),
}

/// Find all documents that reference a given document, filtered to those the
/// viewer may access.
///
/// Access is resolved once per owner collection/global (not per field or per
/// row): collections route through the shared cross-axis visibility filter,
/// globals through a boolean `read` check. Referrers the viewer cannot see are
/// dropped and recorded only via the non-quantified `has_inaccessible` flag.
///
/// # Errors
///
/// Returns a [`ServiceError`] if the scan, an access hook, or a visibility
/// query fails.
pub fn find_back_references(
    ctx: &ServiceContext,
    registry: &Registry,
    target_id: &str,
    locale_config: &LocaleConfig,
) -> Result<BackReferenceReport, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let hooks = ctx.read_hooks()?;

    let raw =
        query::find_back_references(conn.as_ref(), registry, ctx.slug, target_id, locale_config)?;

    let mut references = Vec::with_capacity(raw.len());
    let mut has_inaccessible = false;

    // One access resolution per owner (collection/global), memoized so multiple
    // referring fields of the same owner don't re-run the access hooks.
    let mut cache: HashMap<(String, bool), Visibility> = HashMap::new();

    for group in raw {
        let key = (group.owner_slug.clone(), group.is_global);
        if !cache.contains_key(&key) {
            let visibility = resolve_owner_visibility(hooks, ctx, registry, &group)?;
            cache.insert(key.clone(), visibility);
        }

        match keep_visible_group(conn.as_ref(), registry, group, &cache[&key])? {
            GroupOutcome::Kept(g) => references.push(g),
            GroupOutcome::Partial(g) => {
                references.push(g);
                has_inaccessible = true;
            }
            GroupOutcome::Dropped => has_inaccessible = true,
        }
    }

    Ok(BackReferenceReport {
        references,
        has_inaccessible,
    })
}

/// Resolve how much of one owner (collection or global) the viewer may see.
fn resolve_owner_visibility(
    hooks: &dyn ReadHooks,
    ctx: &ServiceContext,
    registry: &Registry,
    group: &BackReference,
) -> Result<Visibility, ServiceError> {
    if group.is_global {
        return global_visibility(hooks, ctx, registry, &group.owner_slug);
    }

    let Some(def) = registry.get_collection(&group.owner_slug) else {
        return Ok(Visibility::Hidden);
    };

    let read_ctx = ReadAccessCtx {
        def,
        slug: &group.owner_slug,
        user: ctx.user,
        id: None,
        locale: None,
        operation: "find",
        ui_locale: None,
    };

    Ok(match resolve_visibility_filter(hooks, &read_ctx)? {
        None => Visibility::Hidden,
        Some(filters) if filters.is_empty() => Visibility::AllVisible,
        Some(filters) => Visibility::Filtered(filters),
    })
}

/// Globals are a single row gated by a boolean `read` rule — no status,
/// lifecycle, or row constraints apply. Anything other than `Allowed` hides it.
fn global_visibility(
    hooks: &dyn ReadHooks,
    ctx: &ServiceContext,
    registry: &Registry,
    slug: &str,
) -> Result<Visibility, ServiceError> {
    let Some(def) = registry.get_global(slug) else {
        return Ok(Visibility::Hidden);
    };

    let result = hooks.check_access(&AccessCheckInput {
        access: def.access.read.as_ref(),
        user: ctx.user,
        id: None,
        data: None,
        locale: None,
        operation: "find",
        collection: slug,
        ui_locale: None,
    })?;

    Ok(match result {
        AccessResult::Allowed => Visibility::AllVisible,
        _ => Visibility::Hidden,
    })
}

/// Outcome of applying a visibility decision to one referrer group.
enum GroupOutcome {
    /// Every referrer in the group is visible.
    Kept(BackReference),
    /// Some referrers were filtered out; the rest are visible.
    Partial(BackReference),
    /// No referrer in the group is visible.
    Dropped,
}

/// Apply `visibility` to one referrer group, narrowing its document ids.
fn keep_visible_group(
    conn: &dyn DbConnection,
    registry: &Registry,
    group: BackReference,
    visibility: &Visibility,
) -> Result<GroupOutcome, ServiceError> {
    match visibility {
        Visibility::Hidden => Ok(GroupOutcome::Dropped),
        Visibility::AllVisible => Ok(GroupOutcome::Kept(group)),
        Visibility::Filtered(filters) => {
            // Filtered visibility only ever applies to collections (globals are
            // boolean); a missing def means nothing is visible.
            let Some(def) = registry.get_collection(&group.owner_slug) else {
                return Ok(GroupOutcome::Dropped);
            };

            let visible = filter_visible_ids(
                conn,
                &group.owner_slug,
                def,
                &group.document_ids,
                filters,
                None,
            )?;

            let original = group.document_ids.len();
            let kept: Vec<String> = group
                .document_ids
                .into_iter()
                .filter(|id| visible.contains(id))
                .collect();

            if kept.is_empty() {
                return Ok(GroupOutcome::Dropped);
            }

            let narrowed = BackReference::new(
                group.owner_slug,
                group.owner_label,
                group.field_name,
                group.field_label,
                kept,
                group.is_global,
            );

            if narrowed.document_ids.len() < original {
                Ok(GroupOutcome::Partial(narrowed))
            } else {
                Ok(GroupOutcome::Kept(narrowed))
            }
        }
    }
}

/// Find relations in a version snapshot that no longer exist in the registry.
pub fn find_missing_relations(
    conn: &dyn DbConnection,
    registry: &Registry,
    snapshot: &Value,
    fields: &[FieldDefinition],
) -> Vec<MissingRelation> {
    query::find_missing_relations(conn, registry, snapshot, fields)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result;

    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig, LocaleConfig};
    use crate::core::collection::Hooks;
    use crate::core::{
        CollectionDefinition, Document, FieldDefinition, FieldDenial, FieldType, RelationshipConfig,
    };
    use crate::db::{Filter, FilterOp, migrate, pool};
    use crate::hooks::lifecycle::AfterReadCtx;

    /// Read hooks that return a canned access result per owner collection/global
    /// slug — enough to drive `find_back_references`'s access filtering without a
    /// real Lua VM.
    struct MockHooks {
        by_slug: HashMap<String, AccessResult>,
    }

    impl MockHooks {
        fn new(entries: &[(&str, AccessResult)]) -> Self {
            Self {
                by_slug: entries
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), v.clone()))
                    .collect(),
            }
        }
    }

    impl ReadHooks for MockHooks {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<()> {
            Ok(())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(self
                .by_slug
                .get(input.collection)
                .cloned()
                .unwrap_or(AccessResult::Denied))
        }

        fn field_read_denied(
            &self,
            _: &[FieldDefinition],
            _: Option<&Document>,
            _: Option<&str>,
        ) -> Vec<FieldDenial> {
            Vec::new()
        }
    }

    /// media (target) + posts (Upload image -> media, plus an `author` column).
    fn setup() -> (tempfile::TempDir, crate::db::DbPool, Registry) {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("author", FieldType::Text).build(),
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let tmp = tempfile::tempdir().unwrap();
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(media);
            reg.register_collection(posts);
        }
        let locale = LocaleConfig::default();
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &locale).unwrap();

        let conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();
        // Two posts reference m1; one owned by 'keep', one by 'other'.
        conn.execute(
            "INSERT INTO posts (id, image, author) VALUES ('p1', 'm1', 'keep')",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, image, author) VALUES ('p2', 'm1', 'other')",
            &[],
        )
        .unwrap();

        let registry = (*Registry::snapshot(&shared)).clone();
        (tmp, db_pool, registry)
    }

    fn report(
        pool: &crate::db::DbPool,
        registry: &Registry,
        hooks: &dyn ReadHooks,
    ) -> BackReferenceReport {
        let conn = pool.get().unwrap();
        let ctx = ServiceContext::slug_only("media")
            .conn(&conn)
            .read_hooks(hooks)
            .build();
        find_back_references(&ctx, registry, "m1", &LocaleConfig::default()).unwrap()
    }

    /// THE leak regression: an owner collection the viewer cannot read must not
    /// surface its referrers — only the non-quantified flag does.
    #[test]
    fn denied_owner_is_hidden_and_flagged() {
        let (_tmp, pool, registry) = setup();
        let hooks = MockHooks::new(&[("posts", AccessResult::Denied)]);

        let r = report(&pool, &registry, &hooks);
        assert!(r.references.is_empty(), "denied referrers must be hidden");
        assert!(r.has_inaccessible, "hidden referrers set the flag");
    }

    #[test]
    fn allowed_owner_shows_all_referrers() {
        let (_tmp, pool, registry) = setup();
        let hooks = MockHooks::new(&[("posts", AccessResult::Allowed)]);

        let r = report(&pool, &registry, &hooks);
        assert_eq!(r.references.len(), 1);
        assert_eq!(r.references[0].count, 2);
        assert!(!r.has_inaccessible, "nothing hidden");
    }

    #[test]
    fn constrained_owner_shows_only_matching_and_flags_rest() {
        let (_tmp, pool, registry) = setup();
        let hooks = MockHooks::new(&[(
            "posts",
            AccessResult::Constrained(vec![FilterClause::Single(Filter {
                field: "author".to_string(),
                op: FilterOp::Equals("keep".to_string()),
            })]),
        )]);

        let r = report(&pool, &registry, &hooks);
        assert_eq!(r.references.len(), 1);
        assert_eq!(r.references[0].count, 1, "only the 'keep'-owned post");
        assert!(r.references[0].document_ids.contains(&"p1".to_string()));
        assert!(
            r.has_inaccessible,
            "the 'other'-owned post was filtered out"
        );
    }
}
