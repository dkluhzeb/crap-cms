//! Preparing a user document for an auth RPC response.
//!
//! `LoginResponse.user` / `MeResponse.user` carry the same contract every
//! other `Document` on the wire does: hydrated join fields, field-level read
//! access applied, API-hidden columns removed. The login and MFA paths load
//! the row through the credential lookups (`find_by_email`,
//! `reload_authenticated_user`), which are raw reads — without this they
//! would ship a `hidden` field or an `access.read`-denied one that `Me`
//! strips.

use tracing::error;

use crate::{
    core::{CollectionDefinition, Document},
    db::{BoxedConnection, LocaleContext, query},
    service::{AppInfra, RunnerReadHooks, ServiceContext, ServiceError, read_own_document},
};

/// Hydrate join fields, then apply the read strips, in place.
///
/// The user document is its own access context: a field rule on an auth
/// collection typically compares `ctx.user` with the row being read.
///
/// # Errors
///
/// Returns the read pipeline's error — a `before_read` abort among them. The
/// document then carries no fields, and the caller fails the response like any
/// read whose `before_read` aborts.
pub(super) fn prepare_user_document(
    infra: &AppInfra,
    def: &CollectionDefinition,
    collection: &str,
    doc: &mut Document,
    conn: &BoxedConnection,
) -> Result<(), ServiceError> {
    let locale_ctx = LocaleContext::default_for(&infra.locale_config);

    if let Err(e) = query::hydrate_document(
        conn,
        collection,
        &def.fields,
        doc,
        None,
        locale_ctx.as_ref(),
    ) {
        // Hydration failure costs join fields, not correctness of the strip
        // below — log and continue rather than fail an otherwise good login.
        error!("user document hydrate error for {collection}: {e:#}");
    }

    // The read pipeline, with the user's own document as its access context:
    // read hooks, upload sizes and field read strips, in the default locale.
    let user = doc.clone();
    let hooks = RunnerReadHooks::new(&infra.hook_runner, conn, Some(&user), None);
    let ctx = ServiceContext::collection(collection, def)
        .conn(conn)
        .read_hooks(&hooks)
        .user(Some(&user))
        .locale_config(Some(&infra.locale_config))
        .build();

    read_own_document(&ctx, doc, locale_ctx.as_ref())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{fs, path::Path, sync::Arc};

    use r2d2::Pool;
    use r2d2_sqlite::SqliteConnectionManager;
    use serde_json::Value;
    use tonic::{Code, Status};

    use super::*;
    use crate::{
        admin::test_support::test_infra,
        config::{CrapConfig, UploadConfig},
        core::{
            FieldDefinition, FieldType, HookRef, Registry, SharedTokenProvider,
            auth::JwtTokenProvider, upload::create_storage,
        },
        db::DbPool,
        hooks::HookRunner,
    };

    /// Infrastructure whose config dir holds `hooks.users.abort`, a hook that
    /// raises.
    fn infra_with_aborting_hook(dir: &Path) -> Arc<AppInfra> {
        fs::write(dir.join("init.lua"), "").unwrap();
        fs::create_dir_all(dir.join("hooks")).unwrap();
        fs::write(
            dir.join("hooks").join("users.lua"),
            "local M = {}\n\nfunction M.abort(ctx)\n    error(\"before_read aborted\")\nend\n\nreturn M\n",
        )
        .unwrap();

        let config = CrapConfig::test_default();
        let registry = Arc::new(Registry::default());
        let hook_runner = HookRunner::builder()
            .config_dir(dir)
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();

        let manager = SqliteConnectionManager::memory();
        let pool = DbPool::from_pool(Pool::builder().max_size(4).build(manager).unwrap());
        let storage = create_storage(dir, &UploadConfig::default()).unwrap();
        let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new("test-secret"));

        test_infra(
            pool,
            registry,
            hook_runner,
            storage,
            token_provider,
            &config,
            dir,
        )
    }

    /// Prepare a user document on a collection whose `before_read` aborts.
    /// Returns the document as the preparation left it, plus the error.
    fn prepare_with_aborting_hook(dir: &Path) -> (Document, ServiceError) {
        let infra = infra_with_aborting_hook(dir);

        let mut def = CollectionDefinition::new("users");
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Text).build(),
            FieldDefinition::builder("secret", FieldType::Text)
                .hidden(true)
                .build(),
        ];
        def.hooks.before_read = vec![HookRef::new("hooks.users.abort")];

        let conn = infra.pool.get().unwrap();
        let mut doc = Document::new("u1".to_string());
        doc.fields
            .insert("email".to_string(), Value::String("a@b.c".to_string()));
        doc.fields
            .insert("secret".to_string(), Value::String("s3cr3t".to_string()));

        let err = prepare_user_document(&infra, &def, "users", &mut doc, &conn)
            .expect_err("a before_read abort must fail the response");

        (doc, err)
    }

    /// A `before_read` abort on the user's own document was logged and
    /// ignored, so `LoginResponse.user` / `MeResponse.user` carried the row
    /// the strips never touched — a `hidden` field included. The abort now
    /// fails the preparation and leaves nothing to ship.
    #[test]
    fn an_aborted_read_fails_without_shipping_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let (doc, _) = prepare_with_aborting_hook(tmp.path());

        assert!(
            doc.fields.is_empty(),
            "fields survived a before_read abort: {:?}",
            doc.fields
        );
    }

    /// The auth RPCs report this failure the way an ordinary read of the same
    /// collection with the same failing hook does: the hook's message as
    /// `INVALID_ARGUMENT`. Mapping it to `INTERNAL` hid the cause and told clients
    /// to retry a request that can only fail again.
    #[test]
    fn an_aborted_read_maps_to_invalid_argument() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, err) = prepare_with_aborting_hook(tmp.path());

        let status = Status::from(err.reclassify("sqlite"));

        assert_eq!(status.code(), Code::InvalidArgument, "{status:?}");
        assert!(
            status.message().contains("before_read aborted"),
            "the hook's message must survive: {status:?}"
        );
    }
}
