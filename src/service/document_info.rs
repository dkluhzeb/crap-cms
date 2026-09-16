//! Document information service — ref counts, back-references, missing relations.
//!
//! Thin service wrappers for consistency. All future surfaces should call these
//! instead of the query layer directly.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{Document, Registry, document::VersionSnapshot, upload::snapshot_file_keys},
    db::{
        AccessResult, DbConnection, FilterClause, LocaleContext, ops,
        query::{self, BackReference, MissingRelation, filter_visible_ids},
    },
    hooks::AccessCheckInput,
    service::{
        ReadAccessCtx, ReadHooks, ReadStripArgs, ServiceContext, ServiceError, find_stored_version,
        helpers::{enforce_access_constraints, strip_unreadable},
        read_version_snapshot, resolve_visibility_filter,
    },
};

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
/// `target_read` is the caller's already-resolved `read` access result for the
/// target collection (the caller's entry gate handles `Denied`/`Allowed` plus
/// the `default_deny` default). When it is `Constrained`, the row filter is
/// enforced against the target row first: a viewer whose `read` is row-scoped
/// (e.g. `author = me`) must not learn whether referrers exist for a target
/// they cannot actually read — the referrer list is access-filtered regardless,
/// but this closes the existence side-channel on the target itself.
///
/// Owner access is then resolved once per owner collection/global (not per field
/// or per row): collections route through the shared cross-axis visibility
/// filter, globals through a boolean `read` check. Referrers the viewer cannot
/// see are dropped and recorded only via the non-quantified `has_inaccessible`
/// flag.
///
/// # Errors
///
/// Returns [`ServiceError::AccessDenied`] when a `Constrained` `target_read`
/// does not match the target row, or another [`ServiceError`] if the scan, an
/// access hook, or a visibility query fails.
pub fn find_back_references(
    ctx: &ServiceContext,
    registry: &Registry,
    target_id: &str,
    target_read: &AccessResult,
    locale_config: &LocaleConfig,
) -> Result<BackReferenceReport, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let hooks = ctx.read_hooks()?;

    enforce_access_constraints(ctx, target_id, target_read, "read", false)?;

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

    let result = hooks.check_access(
        &AccessCheckInput::builder("find", slug)
            .access(def.access.read.as_ref())
            .user(ctx.user)
            .build(),
    )?;

    Ok(match result {
        AccessResult::Allowed => Visibility::AllVisible,
        AccessResult::Constrained(_) => {
            // Globals are single-row, so a row-filter is meaningless. Every other
            // global path surfaces this misconfiguration loudly (get_global
            // errors, subscribe warns + drops, versions rejects it). Don't fold
            // it silently into "hidden" — an operator debugging why a global's
            // back-references vanished gets no breadcrumb otherwise.
            warn!(
                global = %slug,
                "global `read` access returned a row filter (constrained); globals \
                 are boolean-only — treating as hidden. Return true/false instead."
            );
            Visibility::Hidden
        }
        AccessResult::Denied => Visibility::Hidden,
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

/// What restoring one version would not find any more.
///
/// Both halves are warnings, never blocks: the restore still runs, and the
/// confirmation page says what it would leave dangling.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VersionGaps {
    /// Relations the restore would write whose target documents are gone.
    pub relations: Vec<MissingRelation>,
    /// Storage keys the snapshot names whose bytes the backend no longer
    /// holds — a file a later write replaced and the cleanup removed.
    pub files: Vec<String>,
}

/// A version as the viewer reads it, plus what its restore would no longer
/// find.
///
/// The relation check inspects what a restore writes — the stored snapshot,
/// every locale it records — read the way the viewer reads it: one view per
/// locale, holding that locale's own values, with the read-denied and hidden
/// fields stripped as a version read strips them. A value the viewer cannot
/// read is never reported. `None` when the version does not exist or is hidden
/// from the viewer.
///
/// # Errors
///
/// Returns the version read's errors (access denied, hook errors), or a
/// backend error if the stored snapshot cannot be read.
pub fn version_restore_gaps(
    ctx: &ServiceContext,
    registry: &Registry,
    version_id: &str,
) -> Result<Option<(VersionSnapshot, VersionGaps)>, ServiceError> {
    // The gated read row, still carrying the STORED snapshot: the views and the
    // file keys below are what a restore would write, so both must be taken
    // before the read shaping rewrites the snapshot into the document a read
    // returns.
    let Some(mut version) = find_stored_version(ctx, version_id)? else {
        return Ok(None);
    };

    let views = readable_locale_views(ctx, &version)?;
    let files = missing_snapshot_files(ctx, &version.snapshot);

    // The per-locale `views` above already cover every locale the restore
    // would write; the returned snapshot is the default-locale read shape.
    read_version_snapshot(ctx, ctx.read_hooks()?, &mut version, None)?;

    let conn = ctx.resolve_conn()?;
    let relations = query::find_missing_relations(conn.as_ref(), registry, &views, ctx.fields()?);

    Ok(Some((version, VersionGaps { relations, files })))
}

/// The files a restore of this snapshot would leave dangling: the storage keys
/// it names whose bytes the backend no longer holds.
///
/// The file columns are server-derived and not localized, so they are read from
/// the snapshot as stored rather than per locale. Nothing to report without an
/// upload collection or without a storage backend to ask.
///
/// A backend that cannot answer for a key reports it present: existence is a
/// membership question, and a transient backend failure must not turn into a
/// confident "this file is gone" on the confirmation page.
fn missing_snapshot_files(ctx: &ServiceContext, snapshot: &Value) -> Vec<String> {
    let Some(storage) = ctx.storage.as_ref() else {
        return Vec::new();
    };

    let Some(upload) = ctx
        .collection_def()
        .ok()
        .and_then(|def| def.upload.as_ref())
        .filter(|upload| upload.enabled)
    else {
        return Vec::new();
    };

    let mut missing: Vec<String> = snapshot_file_keys(snapshot, upload)
        .into_iter()
        .filter(|key| !storage.exists(key).unwrap_or(true))
        .collect();

    // A size url and its format variants can name the same key; report each
    // missing file once, in a stable order.
    missing.sort();
    missing.dedup();

    missing
}

/// Each locale's view of a stored snapshot as the viewer reads it: resolved for
/// that locale alone, then stripped of the fields a read withholds —
/// read-denied ones judged in that locale, and hidden ones.
fn readable_locale_views(
    ctx: &ServiceContext,
    stored: &VersionSnapshot,
) -> Result<Vec<Document>, ServiceError> {
    let hooks = ctx.read_hooks()?;
    let fields = ctx.fields()?;
    let parent = stored.parent.to_string();

    let mut views = Vec::new();

    for locale_ctx in exact_locale_contexts(ctx.locale_config) {
        let Some(mut view) =
            ops::snapshot_read_document(&parent, &stored.snapshot, fields, locale_ctx.as_ref())?
        else {
            continue;
        };

        let locale = locale_ctx.as_ref().map(LocaleContext::access_locale);
        strip_unreadable(
            hooks,
            &ReadStripArgs::builder(fields, ctx.slug)
                .user(ctx.user)
                .locale(locale)
                .build(),
            &mut view,
        );

        views.push(view);
    }

    Ok(views)
}

/// One exact context per configured locale: a restore writes each locale's own
/// value, never its fallback's. A single `None` without localization.
fn exact_locale_contexts(config: Option<&LocaleConfig>) -> Vec<Option<LocaleContext>> {
    let Some(config) = config.filter(|c| c.is_enabled()) else {
        return vec![None];
    };

    config
        .locales
        .iter()
        .map(|locale| Some(LocaleContext::exact(config, locale)))
        .collect()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{fs, sync::Arc};

    use anyhow::Result;
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        config::{CrapConfig, DatabaseConfig},
        core::{
            CollectionDefinition, DocumentFields, FieldDefinition, FieldType, HookRef,
            RelationshipConfig, ReqContext, SharedStorage,
            collection::{Hooks, VersionsConfig},
            upload::{CollectionUpload, ImageSizeBuilder, storage::LocalStorage},
        },
        db::{DbPool, Filter, FilterOp, migrate, pool},
        hooks::lifecycle::{AfterReadCtx, access::strip_read_access_data_aware},
        service::FieldReadStrip,
    };

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
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
            Ok(ReqContext::new())
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
    }

    impl FieldReadStrip for MockHooks {}

    /// media (target) + posts (Upload image -> media, plus an `author` column).
    fn setup() -> (tempfile::TempDir, DbPool, Registry) {
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

    fn report(pool: &DbPool, registry: &Registry, hooks: &dyn ReadHooks) -> BackReferenceReport {
        let conn = pool.get().unwrap();
        let media_def = registry.get_collection("media").unwrap();
        let ctx = ServiceContext::collection("media", media_def)
            .conn(&conn)
            .read_hooks(hooks)
            .build();

        // Target read is unconstrained here — these tests exercise owner-side
        // filtering, not the target gate.
        find_back_references(
            &ctx,
            registry,
            "m1",
            &AccessResult::Allowed,
            &LocaleConfig::default(),
        )
        .unwrap()
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

    /// Regression: a row-scoped `read` rule on the TARGET must gate the target
    /// itself — a viewer who can only read their own rows must not learn whether
    /// referrers exist for a target they cannot read (existence side-channel).
    #[test]
    fn constrained_target_read_gates_the_target_itself() {
        let (_tmp, pool, registry) = setup();
        let conn = pool.get().unwrap();
        let hooks = MockHooks::new(&[]); // owner access irrelevant — the gate fires first
        let posts_def = registry.get_collection("posts").unwrap();
        let ctx = ServiceContext::collection("posts", posts_def)
            .conn(&conn)
            .read_hooks(&hooks)
            .build();

        // p1 is owned by 'keep'. A viewer scoped to author='other' probing p1's
        // back-refs is denied outright, not handed an empty list.
        let other_only = AccessResult::Constrained(vec![FilterClause::Single(Filter {
            field: "author".to_string(),
            op: FilterOp::Equals("other".to_string()),
        })]);
        let err =
            find_back_references(&ctx, &registry, "p1", &other_only, &LocaleConfig::default())
                .expect_err("constrained read not matching the target must deny");
        assert!(matches!(err, ServiceError::AccessDenied(_)));

        // The owner ('keep') passes the target gate; the scan then runs (no
        // referrers point at p1, so the report is simply empty).
        let keep_only = AccessResult::Constrained(vec![FilterClause::Single(Filter {
            field: "author".to_string(),
            op: FilterOp::Equals("keep".to_string()),
        })]);
        let r = find_back_references(&ctx, &registry, "p1", &keep_only, &LocaleConfig::default())
            .expect("owner passes the target gate");
        assert!(r.references.is_empty());
        assert!(!r.has_inaccessible);
    }

    /// Read hooks that allow every access check and strip every field whose
    /// read rule is `deny`.
    struct DenyMarkedFields;

    impl ReadHooks for DenyMarkedFields {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, _: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }
    }

    impl FieldReadStrip for DenyMarkedFields {
        fn strip_read_access_map(
            &self,
            fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _document: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            _locale: Option<&str>,
        ) {
            strip_read_access_data_aware(fields, level, &|hook, _data| hook.reference() == "deny");
        }
    }

    /// Locales `en` (default) and `de`.
    fn en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    /// An upload field into `media`.
    fn media_upload(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build()
    }

    /// A versioned `posts` collection with `fields`, synced under `en_de`
    /// beside `authors`, `tags` and `media`; post `p1` plus one existing
    /// target in each (`a1`, `t1`, `m1`).
    fn localized_posts(fields: Vec<FieldDefinition>) -> (tempfile::TempDir, DbPool, Registry) {
        let mut posts = CollectionDefinition::new("posts");
        posts.versions = Some(VersionsConfig::new(false, 0));
        posts.fields = fields;

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
            for slug in ["authors", "tags", "media"] {
                reg.register_collection(CollectionDefinition::new(slug));
            }
            reg.register_collection(posts);
        }
        let registry = (*Registry::snapshot(&shared)).clone();
        migrate::sync_all(&db_pool, &registry, &en_de()).unwrap();

        let conn = db_pool.get().unwrap();
        for (table, id) in [
            ("posts", "p1"),
            ("authors", "a1"),
            ("tags", "t1"),
            ("media", "m1"),
        ] {
            conn.execute(
                &format!("INSERT INTO \"{table}\" (id) VALUES ('{id}')"),
                &[],
            )
            .unwrap();
        }
        drop(conn);

        (tmp, db_pool, registry)
    }

    /// The missing relations a viewer with `hooks` sees on a version of `p1`
    /// stored as `snapshot`.
    fn missing_for(
        db_pool: &DbPool,
        registry: &Registry,
        hooks: &dyn ReadHooks,
        snapshot: &Value,
    ) -> Vec<MissingRelation> {
        let conn = db_pool.get().unwrap();
        let version = query::create_version(&conn, "posts", "p1", "published", snapshot).unwrap();
        let def = registry.get_collection("posts").unwrap();
        let locale = en_de();
        let ctx = ServiceContext::collection("posts", def)
            .conn(&conn)
            .read_hooks(hooks)
            .locale_config(Some(&locale))
            .build();

        let (_, gaps) = version_restore_gaps(&ctx, registry, &version.id)
            .unwrap()
            .expect("the version is visible");

        gaps.relations
    }

    /// Each reported field's sorted missing ids and total id count.
    fn by_field(missing: &[MissingRelation]) -> HashMap<&str, (Vec<&str>, usize)> {
        missing
            .iter()
            .map(|m| {
                let mut ids: Vec<&str> = m.missing_ids.iter().map(String::as_str).collect();
                ids.sort_unstable();

                (m.field_name.as_str(), (ids, m.total_ids))
            })
            .collect()
    }

    /// A stored snapshot records a localized reference once per locale
    /// (`author__de`, `tags__de`, `slides__de`, `meta__hero__de`) and a restore
    /// writes every locale back. The scan read only the bare key, so a target
    /// missing in a non-default locale went unreported. Each field is reported
    /// once, its ids combined across locales.
    #[test]
    fn targets_missing_in_a_non_default_locale_are_reported() {
        let mut hero = media_upload("hero");
        hero.localized = true;

        let (_tmp, db_pool, registry) = localized_posts(vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("authors", false))
                .localized(true)
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .localized(true)
                .build(),
            FieldDefinition::builder("slides", FieldType::Array)
                .localized(true)
                .fields(vec![media_upload("image")])
                .build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![hero])
                .build(),
        ]);

        let snapshot = json!({
            "author": "a1", "author__en": "a1", "author__de": "a_gone",
            "tags": ["t1"], "tags__en": ["t1", "t_gone"], "tags__de": ["t_gone"],
            "slides": [{ "image": "m1" }],
            "slides__en": [{ "image": "m1" }],
            "slides__de": [{ "image": "m_gone" }],
            "meta": { "hero": "m1" }, "meta__hero__en": "m1", "meta__hero__de": "m_gone"
        });
        let missing = missing_for(&db_pool, &registry, &NoStripHooks, &snapshot);

        assert_eq!(
            by_field(&missing),
            HashMap::from([
                ("author", (vec!["a_gone"], 2)),
                ("tags", (vec!["t_gone"], 2)),
                ("slides.image", (vec!["m_gone"], 2)),
                ("meta.hero", (vec!["m_gone"], 2)),
            ]),
            "{missing:?}"
        );
    }

    /// The restore-confirm page listed missing relations for every field of the
    /// stored snapshot, so a viewer learned the names and referenced ids of
    /// fields they may not read. A read-denied or hidden field stays
    /// unreported in every locale.
    #[test]
    fn fields_the_viewer_cannot_read_are_not_reported_in_any_locale() {
        let mut private_image = media_upload("private_image");
        private_image.localized = true;
        private_image.access.read = Some(HookRef::new("deny"));

        let mut internal_image = media_upload("internal_image");
        internal_image.localized = true;
        internal_image.hidden = true;

        let (_tmp, db_pool, registry) =
            localized_posts(vec![media_upload("image"), private_image, internal_image]);

        let snapshot = json!({
            "image": "m_gone",
            "private_image": "m1",
            "private_image__en": "m1",
            "private_image__de": "m_private_gone",
            "internal_image": "m1",
            "internal_image__en": "m1",
            "internal_image__de": "m_internal_gone"
        });
        let missing = missing_for(&db_pool, &registry, &DenyMarkedFields, &snapshot);

        let names: Vec<&str> = missing.iter().map(|m| m.field_name.as_str()).collect();
        assert_eq!(names, vec!["image"], "{missing:?}");
    }

    /// A restore rewrites the snapshot's file urls onto the row, so the
    /// confirmation has to say which of those files storage no longer holds.
    /// Every url column the snapshot carries is checked — the main file and
    /// each size — and a file that is still there is not reported.
    #[test]
    fn a_version_naming_a_deleted_file_reports_it_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let uploads = tmp.path().join("uploads").join("media");
        fs::create_dir_all(&uploads).unwrap();
        fs::write(uploads.join("here.png"), b"bytes").unwrap();

        let storage: SharedStorage = Arc::new(LocalStorage::new(tmp.path().join("uploads")));

        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];

        let mut media = CollectionDefinition::new("media");
        media.upload = Some(upload);
        media.versions = Some(VersionsConfig::new(false, 0));

        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(media);
        let registry = (*Registry::snapshot(&shared)).clone();
        migrate::sync_all(&db_pool, &registry, &LocaleConfig::default()).unwrap();

        let conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO \"media\" (id) VALUES ('m1')", &[])
            .unwrap();

        let snapshot = json!({
            "url": "/uploads/media/gone.png",
            "filename": "gone.png",
            "thumbnail_url": "/uploads/media/here.png",
        });
        let version = query::create_version(&conn, "media", "m1", "published", &snapshot).unwrap();

        let def = registry.get_collection("media").unwrap();
        let hooks = NoStripHooks;
        let ctx = ServiceContext::collection("media", def)
            .conn(&conn)
            .read_hooks(&hooks)
            .storage(Some(storage))
            .build();

        let (_, gaps) = version_restore_gaps(&ctx, &registry, &version.id)
            .unwrap()
            .expect("the version is visible");

        assert_eq!(gaps.files, vec!["media/gone.png".to_string()], "{gaps:?}");
    }

    /// Without a storage backend there is nothing to ask, so the report stays
    /// empty rather than claiming every file is gone.
    #[test]
    fn a_context_without_storage_reports_no_missing_files() {
        let mut media = CollectionDefinition::new("media");
        media.upload = Some(CollectionUpload::new());

        let ctx = ServiceContext::collection("media", &media).build();
        let snapshot = json!({ "url": "/uploads/media/gone.png" });

        assert!(missing_snapshot_files(&ctx, &snapshot).is_empty());
    }

    /// Read hooks that allow every access check and strip nothing.
    struct NoStripHooks;

    impl ReadHooks for NoStripHooks {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, _: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }
    }

    impl FieldReadStrip for NoStripHooks {}
}
