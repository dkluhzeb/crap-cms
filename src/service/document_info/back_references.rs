//! Incoming references to a document: its reference count and the
//! access-filtered report of the documents that hold it.

use std::{
    collections::{HashMap, HashSet},
    slice,
};

use serde::Serialize;
use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{Document, Registry},
    db::{
        AccessResult, DbConnection, FilterClause, LocaleContext,
        query::{self, BackReference, filter_visible_ids},
    },
    hooks::AccessCheckInput,
    service::{
        ReadAccessCtx, ReadHooks, ServiceContext, ServiceError,
        helpers::enforce_access_constraints, resolve_visibility_filter, unreadable_query_paths,
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

/// Everything an owner-side access decision of one back-reference report is
/// judged under: the viewer, the hooks answering for them, and the locale.
///
/// The locale is the default locale's context (a back-reference report has
/// no request locale): field `read` rules see its access locale — the same
/// rule a filter on the referring field and a bulk write's gate apply — and
/// visibility filters resolve localized columns through it.
struct OwnerScan<'a> {
    hooks: &'a dyn ReadHooks,
    registry: &'a Registry,
    user: Option<&'a Document>,
    locale_ctx: Option<&'a LocaleContext>,
}

impl OwnerScan<'_> {
    /// The locale `read` rules are evaluated in.
    fn access_locale(&self) -> Option<&str> {
        self.locale_ctx.map(LocaleContext::access_locale)
    }

    /// Whether the viewer may read the field through which `group`'s
    /// documents reference the target, judged in its owner by the rule that
    /// guards a filter on it ([`unreadable_query_paths`]): the report says
    /// "these documents hold the target in this field", exactly what a filter
    /// on it would answer. An owner no longer defined reads as unreadable
    /// (fail-closed).
    fn field_readable(&self, group: &BackReference) -> Result<bool, ServiceError> {
        let slug = group.owner_slug.as_str();

        let owner = if group.is_global {
            let Some(def) = self.registry.get_global(slug) else {
                return Ok(false);
            };
            ServiceContext::global(slug, def)
        } else {
            let Some(def) = self.registry.get_collection(slug) else {
                return Ok(false);
            };
            ServiceContext::collection(slug, def)
        };

        let owner = owner.read_hooks(self.hooks).user(self.user).build();
        let unreadable = unreadable_query_paths(
            &owner,
            self.access_locale(),
            slice::from_ref(&group.query_path),
        )?;

        Ok(unreadable.is_empty())
    }

    /// Resolve how much of one owner (collection or global) the viewer may see.
    fn owner_visibility(&self, group: &BackReference) -> Result<Visibility, ServiceError> {
        if group.is_global {
            return self.global_visibility(&group.owner_slug);
        }

        let Some(def) = self.registry.get_collection(&group.owner_slug) else {
            return Ok(Visibility::Hidden);
        };

        let read_ctx = ReadAccessCtx {
            def,
            slug: &group.owner_slug,
            user: self.user,
            id: None,
            locale: self.access_locale(),
            operation: "find",
            ui_locale: None,
        };

        Ok(match resolve_visibility_filter(self.hooks, &read_ctx)? {
            None => Visibility::Hidden,
            Some(filters) if filters.is_empty() => Visibility::AllVisible,
            Some(filters) => Visibility::Filtered(filters),
        })
    }

    /// Globals are a single row gated by a boolean `read` rule — no status,
    /// lifecycle, or row constraints apply. Anything other than `Allowed`
    /// hides it.
    fn global_visibility(&self, slug: &str) -> Result<Visibility, ServiceError> {
        let Some(def) = self.registry.get_global(slug) else {
            return Ok(Visibility::Hidden);
        };

        let result = self.hooks.check_access(
            &AccessCheckInput::builder("find", slug)
                .access(def.access.read.as_ref())
                .user(self.user)
                .build(),
        )?;

        Ok(match result {
            AccessResult::Allowed => Visibility::AllVisible,
            AccessResult::Constrained(_) => {
                // Globals are single-row, so a row-filter is meaningless. Every
                // other global path surfaces this misconfiguration loudly
                // (get_global errors, subscribe warns + drops, versions rejects
                // it). Don't fold it silently into "hidden" — an operator
                // debugging why a global's back-references vanished gets no
                // breadcrumb otherwise.
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

    /// Apply `visibility` to one referrer group, narrowing its document ids.
    fn keep_visible(
        &self,
        conn: &dyn DbConnection,
        group: BackReference,
        visibility: &Visibility,
    ) -> Result<GroupOutcome, ServiceError> {
        let filters = match visibility {
            Visibility::Hidden => return Ok(GroupOutcome::Dropped),
            Visibility::AllVisible => return Ok(GroupOutcome::Kept(group)),
            Visibility::Filtered(filters) => filters,
        };

        // Filtered visibility only ever applies to collections (globals are
        // boolean); a missing def means nothing is visible.
        let Some(def) = self.registry.get_collection(&group.owner_slug) else {
            return Ok(GroupOutcome::Dropped);
        };

        let visible = filter_visible_ids(
            conn,
            &group.owner_slug,
            def,
            &group.document_ids,
            filters,
            self.locale_ctx,
        )?;

        Ok(narrow_group(group, &visible))
    }
}

/// Keep only the `visible` ids of `group`, reporting whether any were lost.
fn narrow_group(group: BackReference, visible: &HashSet<String>) -> GroupOutcome {
    let original = group.document_ids.len();
    let kept: Vec<String> = group
        .document_ids
        .iter()
        .filter(|id| visible.contains(*id))
        .cloned()
        .collect();

    if kept.is_empty() {
        return GroupOutcome::Dropped;
    }

    let narrowed = group.with_document_ids(kept);

    if narrowed.document_ids.len() < original {
        return GroupOutcome::Partial(narrowed);
    }

    GroupOutcome::Kept(narrowed)
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
/// A group whose referring field the viewer may not read in its owner — a
/// `hidden` field, or one whose `access.read` rule (or a container's on its
/// path) denies without row data — is dropped: listing it would reveal the
/// field's value for those documents. Owner access is then resolved once per
/// owner collection/global (not per field or per row): collections route
/// through the shared cross-axis visibility filter, globals through a boolean
/// `read` check. Referrers the viewer cannot see are dropped and recorded only
/// via the non-quantified `has_inaccessible` flag. Every access decision is
/// made in the default locale of `locale_config`.
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

    enforce_access_constraints(ctx, target_id, target_read, "read", false)?;

    let raw =
        query::find_back_references(conn.as_ref(), registry, ctx.slug, target_id, locale_config)?;

    let default_locale = LocaleContext::default_for(locale_config);
    let scan = OwnerScan {
        hooks: ctx.read_hooks()?,
        registry,
        user: ctx.user,
        locale_ctx: default_locale.as_ref(),
    };

    let mut references = Vec::with_capacity(raw.len());
    let mut has_inaccessible = false;

    // One access resolution per owner (collection/global), memoized so multiple
    // referring fields of the same owner don't re-run the access hooks.
    let mut cache: HashMap<(String, bool), Visibility> = HashMap::new();

    for group in raw {
        if !scan.field_readable(&group)? {
            has_inaccessible = true;
            continue;
        }

        let key = (group.owner_slug.clone(), group.is_global);
        if !cache.contains_key(&key) {
            cache.insert(key.clone(), scan.owner_visibility(&group)?);
        }

        match scan.keep_visible(conn.as_ref(), group, &cache[&key])? {
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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result;
    use serde_json::{Map, Value};

    use super::*;
    use crate::{
        config::{CrapConfig, DatabaseConfig},
        core::{
            CollectionDefinition, DocumentFields, FieldAccess, FieldDefinition, FieldType, HookRef,
            RelationshipConfig, ReqContext, collection::Hooks,
        },
        db::{DbPool, Filter, FilterOp, migrate, pool},
        hooks::lifecycle::{AfterReadCtx, access::strip_read_access_data_aware},
        service::{FieldReadStrip, document_info::test_support::DenyMarkedFields},
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

    fn denied(mut field: FieldDefinition) -> FieldDefinition {
        field.access = FieldAccess {
            read: Some(HookRef::new("deny")),
            ..Default::default()
        };

        field
    }

    /// `posts` referencing `media/m1` through a plain field (`cover`), a
    /// read-denied field (`image`), a hidden field (`secret`), a field inside
    /// a read-denied group (`meta.hero`) and a read-denied array row field
    /// (`slides.pic`) — one post each.
    fn gated_referrers() -> (tempfile::TempDir, DbPool, Registry) {
        let media = CollectionDefinition::new("media");

        let upload = |name: &str| {
            FieldDefinition::builder(name, FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build()
        };

        let mut secret = upload("secret");
        secret.hidden = true;

        let meta = FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![upload("hero")])
            .build();
        let slides = FieldDefinition::builder("slides", FieldType::Array)
            .fields(vec![denied(upload("pic"))])
            .build();

        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            upload("cover"),
            denied(upload("image")),
            secret,
            denied(meta),
            slides,
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
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &LocaleConfig::default()).unwrap();

        let conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
            .unwrap();

        for (id, column) in [
            ("p1", "cover"),
            ("p2", "image"),
            ("p3", "secret"),
            ("p4", "meta__hero"),
        ] {
            conn.execute(
                &format!("INSERT INTO posts (id, {column}) VALUES ('{id}', 'm1')"),
                &[],
            )
            .unwrap();
        }

        conn.execute("INSERT INTO posts (id) VALUES ('p5')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, pic) VALUES ('s1', 'p5', 0, 'm1')",
            &[],
        )
        .unwrap();
        drop(conn);

        let registry = (*Registry::snapshot(&shared)).clone();
        (tmp, db_pool, registry)
    }

    /// Regression: the report listed every referring field whose owner
    /// document the viewer could read, so it revealed which documents hold the
    /// target in a field the viewer may not read — a hidden field, a
    /// read-denied one, or one inside a read-denied group or array row. Those
    /// groups are dropped (only the non-quantified flag remains).
    #[test]
    fn referrers_through_an_unreadable_field_are_hidden_and_flagged() {
        let (_tmp, pool, registry) = gated_referrers();

        let r = report(&pool, &registry, &DenyMarkedFields);

        let fields: Vec<&str> = r.references.iter().map(|g| g.field_name.as_str()).collect();
        assert_eq!(fields, vec!["cover"], "only the readable field is reported");
        assert_eq!(r.references[0].document_ids, vec!["p1".to_string()]);
        assert!(r.has_inaccessible, "the hidden referrers set the flag");
    }

    /// Without field rules every referring field is reported, by its full path.
    #[test]
    fn referrers_through_readable_fields_are_all_reported() {
        let (_tmp, pool, registry) = gated_referrers();

        let hooks = MockHooks::new(&[("posts", AccessResult::Allowed)]);
        let r = report(&pool, &registry, &hooks);

        let mut fields: Vec<&str> = r.references.iter().map(|g| g.field_name.as_str()).collect();
        fields.sort_unstable();

        // `secret` is hidden for everyone; MockHooks strips no read rule.
        assert_eq!(fields, vec!["cover", "image", "meta.hero", "slides.pic"]);
        assert!(r.has_inaccessible);
    }

    /// `posts` rows readable only when `title = keep`; the `image` field's
    /// `read` rule (`en-only`) admits only a reader in the `en` locale.
    struct EnglishOnlyImage;

    impl ReadHooks for EnglishOnlyImage {
        fn before_read(&self, _: &Hooks, _: &str, _: &str, _: Option<&str>) -> Result<ReqContext> {
            Ok(ReqContext::new())
        }

        fn after_read_one(&self, _: &AfterReadCtx, doc: Document) -> Document {
            doc
        }

        fn check_access(&self, _: &AccessCheckInput<'_>) -> Result<AccessResult> {
            Ok(AccessResult::Constrained(vec![FilterClause::Single(
                Filter {
                    field: "owner".to_string(),
                    op: FilterOp::Equals("keep".to_string()),
                },
            )]))
        }
    }

    impl FieldReadStrip for EnglishOnlyImage {
        fn strip_read_access_map(
            &self,
            fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _document: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            locale: Option<&str>,
        ) {
            strip_read_access_data_aware(fields, level, &|hook, _data| {
                hook.reference() == "en-only" && locale != Some("en")
            });
        }
    }

    /// Regression: on a localized owner the report judged the referring
    /// field's `read` rule with no locale, while filter checks use the default
    /// locale. It now runs in the default locale, next to the owner's row
    /// filter (which names a shared field, as access rules must).
    #[test]
    fn a_localized_owner_is_judged_in_the_default_locale() {
        let locales = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };

        let mut image = FieldDefinition::builder("image", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build();
        image.access = FieldAccess {
            read: Some(HookRef::new("en-only")),
            ..Default::default()
        };

        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("owner", FieldType::Text).build(),
            image,
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
            reg.register_collection(CollectionDefinition::new("media"));
            reg.register_collection(posts);
        }
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &locales).unwrap();
        let registry = (*Registry::snapshot(&shared)).clone();

        let conn = db_pool.get().unwrap();
        conn.execute_batch(
            "INSERT INTO media (id) VALUES ('m1');
             INSERT INTO posts (id, image, title__en, owner) VALUES ('p1', 'm1', 'a', 'keep');
             INSERT INTO posts (id, image, title__en, owner) VALUES ('p2', 'm1', 'b', 'other');",
        )
        .unwrap();

        let ctx = ServiceContext::collection("media", registry.get_collection("media").unwrap())
            .conn(&conn)
            .read_hooks(&EnglishOnlyImage)
            .build();

        let r = find_back_references(&ctx, &registry, "m1", &AccessResult::Allowed, &locales)
            .expect("a localized owner resolves");

        let fields: Vec<&str> = r.references.iter().map(|g| g.field_name.as_str()).collect();
        assert_eq!(fields, vec!["image"], "readable in the default locale");
        assert_eq!(r.references[0].document_ids, vec!["p1".to_string()]);
        assert!(r.has_inaccessible, "the 'other' post is filtered out");
    }
}
