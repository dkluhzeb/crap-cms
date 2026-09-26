//! Whether the caller of a write may see the document the write reports.
//!
//! A write needs only its own access key (`create`, `update`, …), but the
//! document it reports back is content: it sits in one content view — the
//! published view, the draft view, or the trash — and a caller that may not
//! read that view must not read the document through its own write (the
//! merged pending draft a draft save reports can hold other editors' unsaved
//! edits). Such a caller gets only the document's id and the outcome of its
//! write (`_status`, `_revision`).

use serde_json::Value;

use crate::{
    core::{
        CollectionDefinition, Document, HookRef,
        collection::{Access, GlobalDefinition},
    },
    db::{AccessResult, query::filter::memory::matches_document},
    hooks::AccessCheckInput,
    service::{
        Def, ReadAccessCtx, ServiceContext, ServiceError, check_view_by, global_access_allowed,
        hooks::WriteHooks,
    },
};

/// The write-outcome keys an id-only report keeps.
const OUTCOME_KEYS: &[&str] = &["_status", "_revision"];

/// Whether the caller of `ctx` may see `doc` — the document a write reports —
/// in the content view it now sits in, judged through `write_hooks`:
///
/// - a trashed document → the `trash` view (`access.trash ?? update`);
/// - draft content of a definition with drafts (a draft save's snapshot, a
///   draft create, an unpublished document) → the `draft` view
///   (`access.draft ?? update`);
/// - otherwise → the published view (`access.read`).
///
/// A collection view's row constraint is matched against the reported
/// document itself — as a draft read matches its snapshot. A context that
/// overrides access sees everything.
///
/// # Errors
///
/// Propagates an access-hook error, an invalid row constraint, or a filter
/// table returned for a global.
pub(crate) fn reported_view_open(
    write_hooks: &dyn WriteHooks,
    ctx: &ServiceContext,
    doc: &Document,
    locale: Option<&str>,
) -> Result<bool, ServiceError> {
    if ctx.override_access {
        return Ok(true);
    }

    let check = |input: &AccessCheckInput<'_>| write_hooks.check_access(input);

    match ctx.def {
        Def::Collection(def) => collection_view_open(&check, ctx, (def, doc), locale),
        Def::Global(def) => global_view_open(&check, ctx, (def, doc), locale),
        Def::None => Ok(false),
    }
}

/// Reduce `doc` to what a caller who may not see it learns from its own
/// write: the id, `_status` and `_revision`.
pub(crate) fn keep_only_outcome(doc: &mut Document) {
    doc.fields
        .retain(|key, _| OUTCOME_KEYS.contains(&key.as_str()));

    doc.created_at = None;
    doc.updated_at = None;
}

/// [`reported_view_open`] for a collection document.
fn collection_view_open(
    check: &dyn Fn(&AccessCheckInput<'_>) -> anyhow::Result<AccessResult>,
    ctx: &ServiceContext,
    (def, doc): (&CollectionDefinition, &Document),
    locale: Option<&str>,
) -> Result<bool, ServiceError> {
    let read_ctx = ReadAccessCtx {
        def,
        slug: ctx.slug,
        user: ctx.user,
        id: Some(doc.id.as_ref()),
        locale,
        operation: "find_by_id",
        ui_locale: ctx.ui_locale.as_deref(),
    };

    let rule = view_rule(&def.access, def.has_drafts(), doc);

    Ok(match check_view_by(check, &read_ctx, rule)? {
        AccessResult::Allowed => true,
        AccessResult::Denied => false,
        AccessResult::Constrained(filters) => matches_document(doc, &filters, &def.fields),
    })
}

/// [`reported_view_open`] for a global — its views are boolean, as on every
/// global read.
fn global_view_open(
    check: &dyn Fn(&AccessCheckInput<'_>) -> anyhow::Result<AccessResult>,
    ctx: &ServiceContext,
    (def, doc): (&GlobalDefinition, &Document),
    locale: Option<&str>,
) -> Result<bool, ServiceError> {
    let access = check(
        &AccessCheckInput::builder("get", ctx.slug)
            .access(view_rule(&def.access, def.has_drafts(), doc))
            .user(ctx.user)
            .locale(locale)
            .ui_locale(ctx.ui_locale.as_deref())
            .build(),
    )?;

    global_access_allowed(&access, ctx.slug)
}

/// The access rule of the content view `doc` sits in, read from its own
/// lifecycle keys.
fn view_rule<'a>(access: &'a Access, has_drafts: bool, doc: &Document) -> Option<&'a HookRef> {
    let is_set = |key: &str| doc.fields.get(key).is_some_and(|v| !v.is_null());

    if is_set("_deleted_at") {
        return access.resolve_trash();
    }

    let is_draft = doc.fields.get("_status").and_then(Value::as_str) == Some("draft");

    if has_drafts && is_draft {
        return access.resolve_draft();
    }

    access.read.as_ref()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn access() -> Access {
        Access {
            read: Some(HookRef::new("read_fn")),
            draft: Some(HookRef::new("draft_fn")),
            trash: Some(HookRef::new("trash_fn")),
            ..Default::default()
        }
    }

    fn doc(fields: Value) -> Document {
        let Value::Object(map) = fields else {
            panic!("document fields are an object");
        };

        let mut doc = Document::new("d1".to_string());
        doc.fields.extend(map);
        doc
    }

    fn rule_of(has_drafts: bool, fields: Value) -> Option<String> {
        let access = access();
        view_rule(&access, has_drafts, &doc(fields)).map(|r| r.reference().to_string())
    }

    /// A document is judged by the view its content sits in: trashed → trash,
    /// draft content of a drafts definition → draft, otherwise published.
    #[test]
    fn the_view_follows_the_documents_lifecycle() {
        assert_eq!(
            rule_of(true, json!({"_status": "published"})).as_deref(),
            Some("read_fn")
        );
        assert_eq!(
            rule_of(true, json!({"_status": "draft"})).as_deref(),
            Some("draft_fn")
        );
        assert_eq!(
            rule_of(false, json!({"_status": "draft"})).as_deref(),
            Some("read_fn"),
            "no status axis without drafts"
        );
        assert_eq!(
            rule_of(
                true,
                json!({"_deleted_at": "2024-01-01", "_status": "draft"})
            )
            .as_deref(),
            Some("trash_fn")
        );
        assert_eq!(
            rule_of(true, json!({"_deleted_at": null})).as_deref(),
            Some("read_fn")
        );
    }

    /// An id-only report keeps the id and the write's outcome, nothing else.
    #[test]
    fn an_id_only_report_keeps_the_outcome_keys() {
        let mut reported = doc(json!({"title": "Secret", "_status": "draft", "_revision": 3}));
        reported.created_at = Some("2024-01-01".to_string());

        keep_only_outcome(&mut reported);

        assert_eq!(reported.id.as_ref(), "d1");
        assert_eq!(reported.fields.get("title"), None);
        assert_eq!(reported.fields.get("_status"), Some(&json!("draft")));
        assert_eq!(reported.fields.get("_revision"), Some(&json!(3)));
        assert_eq!(reported.created_at, None);
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod write_tests {
    use anyhow::Result as AnyResult;
    use serde_json::{Value, json};

    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, DocumentFields, FieldDefinition, FieldType, HookRef, Hooks,
            Registry, ValidationError, VersionsConfig,
        },
        db::{AccessResult, DbConnection, DbPool, migrate, pool},
        hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
        service::{
            FieldReadStrip, ServiceContext, WriteInput, create_document_in_conn, hooks::WriteHooks,
            update_document_in_conn,
        },
    };

    /// Write hooks running nothing, denying the access rules named in `denied`
    /// and allowing every other.
    struct ViewGate {
        denied: &'static [&'static str],
    }

    impl WriteHooks for ViewGate {
        fn run_before_write(
            &self,
            _: &Hooks,
            _: &[FieldDefinition],
            ctx: HookContext,
            _: &ValidationCtx,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _: &Hooks,
            _: &[FieldDefinition],
            _: HookEvent,
            ctx: HookContext,
            _: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _: &Hooks,
            _: HookEvent,
            ctx: HookContext,
            _: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
            let rule = input.access.map(HookRef::reference).unwrap_or_default();

            Ok(if self.denied.contains(&rule) {
                AccessResult::Denied
            } else {
                AccessResult::Allowed
            })
        }

        fn validate_fields(
            &self,
            _: &[FieldDefinition],
            _: &DocumentFields,
            _: &ValidationCtx,
        ) -> Result<(), ValidationError> {
            Ok(())
        }
    }

    impl FieldReadStrip for ViewGate {}

    /// A drafts `posts` collection: `read` = `staff_only`, `draft` =
    /// `reviewers`, writes open.
    fn migrated_posts() -> (tempfile::TempDir, DbPool, CollectionDefinition) {
        let mut def = CollectionDefinition::new("posts");
        def.versions = Some(VersionsConfig::new(true, 10));
        def.access.read = Some(HookRef::new("staff_only"));
        def.access.draft = Some(HookRef::new("reviewers"));
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("notes", FieldType::Text).build(),
        ];

        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def.clone());
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();

        (tmp, db_pool, def)
    }

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    /// Create `posts/{id}` published, as a writer who sees everything.
    fn published_post(conn: &dyn DbConnection, def: &CollectionDefinition) -> String {
        let open = ViewGate { denied: &[] };
        let ctx = ServiceContext::collection("posts", def)
            .conn(conn)
            .write_hooks(&open)
            .build();

        let (doc, _) = create_document_in_conn(
            &ctx,
            WriteInput::builder(data(&[("title", json!("Live"))])).build(),
        )
        .unwrap();

        doc.id.to_string()
    }

    /// Regression: a writer that may not read the collection read every
    /// field of the document through its own write — the reported document
    /// was stripped only by field rules. A caller outside the published view
    /// gets the id and the write's outcome.
    #[test]
    fn a_writer_without_read_gets_only_the_id_of_what_it_wrote() {
        let (_tmp, db_pool, def) = migrated_posts();
        let conn = db_pool.get().unwrap();
        let id = published_post(&conn, &def);

        let author = ViewGate {
            denied: &["staff_only"],
        };
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&author)
            .build();

        let (created, _) = create_document_in_conn(
            &ctx,
            WriteInput::builder(data(&[("title", json!("Mine"))])).build(),
        )
        .unwrap();
        assert!(!created.id.as_ref().is_empty());
        assert_eq!(created.get_str("title"), None, "create reports no content");
        assert_eq!(created.get_str("_status"), Some("published"));

        let (updated, _) =
            update_document_in_conn(&ctx, &id, WriteInput::builder(data(&[])).build()).unwrap();
        assert_eq!(updated.id.as_ref(), id);
        assert_eq!(updated.get_str("title"), None, "update reports no content");
    }

    /// Regression: a draft save reports the merged pending draft — including
    /// edits other editors saved — to a writer whose `draft` view is closed.
    /// An author without the draft view gets the id and `_status = "draft"`;
    /// a reviewer gets the draft.
    #[test]
    fn a_draft_save_by_an_author_without_the_draft_view_reports_only_the_id() {
        let (_tmp, db_pool, def) = migrated_posts();
        let conn = db_pool.get().unwrap();
        let id = published_post(&conn, &def);

        let reviewer = ViewGate { denied: &[] };
        let reviewer_ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&reviewer)
            .build();
        update_document_in_conn(
            &reviewer_ctx,
            &id,
            WriteInput::builder(data(&[("notes", json!("reviewer's unsaved edit"))]))
                .draft(true)
                .build(),
        )
        .unwrap();

        let author = ViewGate {
            denied: &["reviewers"],
        };
        let author_ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&author)
            .build();
        let (saved, _) = update_document_in_conn(
            &author_ctx,
            &id,
            WriteInput::builder(data(&[("title", json!("Author's draft"))]))
                .draft(true)
                .build(),
        )
        .unwrap();

        assert_eq!(saved.id.as_ref(), id);
        assert_eq!(saved.get_str("_status"), Some("draft"));
        assert_eq!(
            saved.get_str("notes"),
            None,
            "the reviewer's edit stays hidden"
        );
        assert_eq!(saved.get_str("title"), None);

        let (seen, _) = update_document_in_conn(
            &reviewer_ctx,
            &id,
            WriteInput::builder(data(&[])).draft(true).build(),
        )
        .unwrap();
        assert_eq!(seen.get_str("notes"), Some("reviewer's unsaved edit"));
        assert_eq!(seen.get_str("title"), Some("Author's draft"));
    }
}
