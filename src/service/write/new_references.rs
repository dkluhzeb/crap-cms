//! Refusing a write's new references to documents its writer may not read.
//!
//! A write that points a relationship or upload at a document that does not
//! exist (or is in the trash) is refused with
//! [`UnavailableReferences`] — reported on the field holding the reference.
//! A new reference to a live document the writer may not read is refused with
//! the very same error: accepting it would tell the writer that a document it
//! cannot see exists (and would raise that document's reference count, making
//! it undeletable for its owner). The two cases read alike, so the refusal is
//! no existence oracle.

use anyhow::Result;

use crate::{
    core::{CollectionDefinition, Registry},
    db::{
        LocaleContext, ViewScope,
        query::{
            filter_visible_ids,
            ref_count::{AddedReferences, UnavailableReferences},
        },
    },
    hooks::lifecycle::AccessCheckInput,
    service::{ReadAccessCtx, ServiceContext, check_view_by, hooks::WriteHooks},
};

/// Refuse the references `added` holds to documents the writer of `ctx` may
/// not read, with [`UnavailableReferences`] — the error a missing target
/// gets. A document counts as readable when the writer's published view
/// (`read`) or draft view (`draft`, else `update`) admits it, row constraints
/// included. References the document already held are not in `added`, so they
/// are never judged.
///
/// Judged through the write's own hooks, so a write that overrides access —
/// or an internal write without hooks — refuses nothing here.
///
/// # Errors
///
/// [`UnavailableReferences`] for the first target collection holding an
/// unreadable new reference, or an access-hook / backend error.
pub(crate) fn refuse_unreadable_references(
    ctx: &ServiceContext,
    added: &AddedReferences,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    if added.is_empty() || ctx.override_access {
        return Ok(());
    }

    let Some(write_hooks) = ctx.write_hooks else {
        return Ok(());
    };
    let Some(registry) = write_hooks.registry() else {
        return Ok(());
    };

    let judge = ReferenceJudge {
        ctx,
        write_hooks,
        registry,
        locale_ctx,
    };

    for (collection, ids) in added.iter() {
        judge.refuse_unreadable(collection, ids)?;
    }

    Ok(())
}

/// What the new references of one write are judged under. Built in one place
/// with every field set.
struct ReferenceJudge<'a, 'c> {
    ctx: &'a ServiceContext<'c>,
    write_hooks: &'a dyn WriteHooks,
    registry: &'a Registry,
    locale_ctx: Option<&'a LocaleContext>,
}

impl ReferenceJudge<'_, '_> {
    /// Refuse `ids` of `collection` the writer may not read. A collection no
    /// longer registered holds nothing to judge (the reference count already
    /// refused a missing target).
    fn refuse_unreadable(&self, collection: &str, ids: &[String]) -> Result<()> {
        let Some(def) = self.registry.get_collection(collection) else {
            return Ok(());
        };

        let hidden = self.unreadable(collection, def, ids)?;

        if hidden.is_empty() {
            return Ok(());
        }

        Err(UnavailableReferences {
            collection: collection.to_string(),
            ids: hidden,
        }
        .into())
    }

    /// The `ids` of `collection` neither of the writer's live views admits.
    fn unreadable(
        &self,
        collection: &str,
        def: &CollectionDefinition,
        ids: &[String],
    ) -> Result<Vec<String>> {
        let scope = self.live_views(collection, def)?;

        if !scope.is_anything_visible() {
            return Ok(ids.to_vec());
        }

        let filters = scope.into_filters();

        if filters.is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.ctx.resolve_conn()?;
        let visible = filter_visible_ids(
            conn.as_ref(),
            collection,
            def,
            ids,
            &filters,
            self.locale_ctx,
        )?;

        Ok(ids
            .iter()
            .filter(|id| !visible.contains(*id))
            .cloned()
            .collect())
    }

    /// The writer's published ∪ draft view of `collection`, as a read that
    /// opts into drafts resolves it.
    fn live_views(&self, collection: &str, def: &CollectionDefinition) -> Result<ViewScope> {
        let check = |input: &AccessCheckInput<'_>| self.write_hooks.check_access(input);
        let read_ctx = ReadAccessCtx {
            def,
            slug: collection,
            user: self.ctx.user,
            id: None,
            locale: self.locale_ctx.map(LocaleContext::access_locale),
            operation: "find",
            ui_locale: self.ctx.ui_locale.as_deref(),
        };

        let published = check_view_by(&check, &read_ctx, def.access.read.as_ref())?;
        let draft = def
            .has_drafts()
            .then(|| check_view_by(&check, &read_ctx, def.access.resolve_draft()))
            .transpose()?;

        Ok(ViewScope::assemble(
            def.has_drafts(),
            Some(published),
            draft,
        ))
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result as AnyResult;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            DocumentFields, FieldDefinition, FieldType, Hooks, RelationshipConfig, ValidationError,
        },
        db::{AccessResult, DbConnection, DbPool, migrate, pool},
        hooks::{HookContext, HookEvent, ValidationCtx},
        service::{
            FieldReadStrip, ServiceError, WriteInput, create_document_in_conn,
            update_document_in_conn,
        },
    };

    /// Write hooks running nothing, knowing `registry`, and denying every
    /// access check on `authors` when `.1`.
    struct Writer<'a>(&'a Registry, bool);

    impl WriteHooks for Writer<'_> {
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

        fn registry(&self) -> Option<&Registry> {
            Some(self.0)
        }

        fn check_access(&self, input: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
            Ok(if self.1 && input.collection == "authors" {
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

    impl FieldReadStrip for Writer<'_> {}

    /// `posts.author` → `authors`, with author `a1` stored.
    fn migrated() -> (tempfile::TempDir, DbPool, Registry) {
        let authors = CollectionDefinition::new("authors");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("authors", false))
                .build(),
        ];

        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(authors);
            reg.register_collection(posts);
        }
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();

        db_pool
            .get()
            .unwrap()
            .execute("INSERT INTO authors (id) VALUES ('a1')", &[])
            .unwrap();

        let registry = (*Registry::snapshot(&shared)).clone();
        (tmp, db_pool, registry)
    }

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    /// The field errors of a refused write.
    fn field_errors(err: ServiceError) -> Vec<(String, Option<String>)> {
        let ServiceError::Validation(ve) = err else {
            panic!("expected a field error, got {err:?}");
        };

        ve.errors.into_iter().map(|e| (e.field, e.key)).collect()
    }

    /// Regression: a new reference to a live document the writer may not read
    /// was accepted (and raised its reference count), while one to a missing
    /// document was refused — so the answer told a hidden document's
    /// existence. Both are now refused with the same field error; a writer
    /// who may read the target is unaffected, and a reference the document
    /// already holds is never judged.
    #[test]
    fn a_reference_to_an_unreadable_document_reads_like_a_missing_one() {
        let (_tmp, db_pool, registry) = migrated();
        let def = registry.get_collection("posts").unwrap();
        let conn = db_pool.get().unwrap();

        let reader = Writer(&registry, false);
        let reader_ctx = ServiceContext::collection("posts", def)
            .conn(&conn)
            .write_hooks(&reader)
            .build();
        let (post, _) = create_document_in_conn(
            &reader_ctx,
            WriteInput::builder(data(&[("author", json!("a1"))])).build(),
        )
        .expect("a readable target is accepted");

        let outsider = Writer(&registry, true);
        let outsider_ctx = ServiceContext::collection("posts", def)
            .conn(&conn)
            .write_hooks(&outsider)
            .build();

        let hidden = create_document_in_conn(
            &outsider_ctx,
            WriteInput::builder(data(&[("author", json!("a1"))])).build(),
        )
        .unwrap_err();
        let missing = create_document_in_conn(
            &outsider_ctx,
            WriteInput::builder(data(&[("author", json!("nope"))])).build(),
        )
        .unwrap_err();

        assert_eq!(field_errors(hidden), field_errors(missing));

        update_document_in_conn(
            &outsider_ctx,
            post.id.as_ref(),
            WriteInput::builder(data(&[("author", json!("a1")), ("title", json!("t"))])).build(),
        )
        .expect("a reference the document already holds is not judged");
    }
}
