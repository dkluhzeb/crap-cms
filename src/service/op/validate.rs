//! The `validate` (dry-run) operations — collection and global.
//!
//! Runs the write's before-write pipeline (admission, access gate,
//! field-access stripping, field hooks, validators, unique checks,
//! `before_validate` hooks) without persisting and returns the typed outcome
//! (`None` = valid, `Some(ValidationError)` = the per-field failures).
//!
//! The input passes the SAME admission prefix the real write runs
//! (`admit_create_input` / `admit_update_input` / `admit_global_update_input`):
//! canonicalized (nested groups, canonical email and text, untrusted upload
//! metadata stripped), the pending draft adopted as the base when the previewed
//! write publishes one (never on a `draft` dry-run), and the locale lock
//! applied. The adopted draft, stripped by the caller's field-level write
//! access, is the completeness gate's locale overlay, as on the publish itself.
//! Only the row lock is left out — the dry-run writes nothing.
//!
//! Access semantics match the surface's REAL write: the target operation's
//! collection-level access rule (`access.create` / `access.update`) gates the
//! dry-run exactly like the write it previews, judged on the admitted data —
//! an anonymous caller denied the write is denied the dry-run too, closing the
//! unique-collision enumeration channel an ungated validate offered — and the
//! acting user (or MCP's override) drives field-level write-access stripping.

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    core::{DocumentFields, ValidationError},
    db::{DbConnection, LocaleContext, query::helpers::global_table},
    service::{
        Def, PendingDraft, RunnerWriteHooks, ServiceContext, ServiceError, ValidateContext,
        WriteInput, admit_create_input, admit_global_update_input, admit_update_input,
        check_create_access, check_global_update_access, check_update_access, hooks::WriteHooks,
        stored_fields_for_update_rules, stored_global_fields_for_update_rules, validate_document,
    },
};

use crate::core::Builder;

use super::{Operation, locale::write_locale_ctx};

/// Owned arguments for [`Validate`] / [`ValidateGlobal`].
#[derive(Builder)]
pub struct ValidateArgs {
    #[builder(required)]
    pub data: DocumentFields,
    pub locale_ctx: Option<LocaleContext>,
    /// Update-mode target id, excluded from unique checks. `None` = create
    /// mode. Ignored by [`ValidateGlobal`] (always update against `default`).
    pub exclude_id: Option<String>,
    /// Validate as a draft write (skips required-field checks where the
    /// target supports drafts — the body clamps, like the real write path —
    /// and adopts no pending draft, like a draft save).
    pub draft: bool,
    /// The previewed write carries server-derived upload metadata of its own
    /// (the admin multipart upload path, whose real write is trusted). Every
    /// other surface leaves it false, so caller-supplied `url` / `filename` /
    /// size columns are stripped exactly as their real write strips them.
    #[builder(default = false)]
    pub trusted_upload_metadata: bool,
}

/// The dry-run outcome: `None` = valid; `Some(err)` = the typed validation
/// failure. Kept typed (not pre-flattened to a field map) so the admin codec
/// can translate messages via i18n while API codecs flatten with
/// [`ValidationError::to_field_map`]. Non-validation failures propagate as
/// `Err(ServiceError)`.
pub type ValidateOutput = Option<ValidationError>;

/// Where the dry-run judges: the write hooks and the connection it runs on.
#[derive(Clone, Copy)]
struct DryRun<'a> {
    write_hooks: &'a dyn WriteHooks,
    conn: &'a dyn DbConnection,
}

impl<'a> DryRun<'a> {
    fn new(write_hooks: &'a dyn WriteHooks, conn: &'a dyn DbConnection) -> Self {
        Self { write_hooks, conn }
    }
}

/// The input after the admission prefix, with the pending draft it adopted.
struct Admitted<'i> {
    input: WriteInput<'i>,
    pending_draft: PendingDraft,
}

impl<'i> Admitted<'i> {
    fn new(input: WriteInput<'i>, pending_draft: PendingDraft) -> Self {
        Self {
            input,
            pending_draft,
        }
    }
}

/// Run the dry-run against an assembled [`ValidateContext`].
///
/// **Conn mode** (`ctx.write_hooks` set — Lua inside a hook transaction):
/// runs on the caller's connection with the caller's hooks; side effects of
/// `before_validate` hooks follow the outer commit/rollback.
///
/// **Pool mode** (every other surface): runs inside a transaction that is
/// always ROLLED BACK, so hook side effects during validation are discarded.
fn run_validate(
    ctx: &ServiceContext<'_>,
    vctx: &ValidateContext<'_>,
    args: ValidateArgs,
) -> Result<ValidateOutput, ServiceError> {
    let ValidateArgs {
        data,
        locale_ctx,
        draft,
        trusted_upload_metadata,
        exclude_id: _,
    } = args;

    // The dry-run answers for the write it previews, so it refuses the same
    // locales the write refuses.
    let locale_ctx = write_locale_ctx(locale_ctx)?;

    let input = WriteInput::builder(data)
        .locale_ctx(locale_ctx.as_ref())
        .draft(draft)
        .ui_locale(ctx.ui_locale.clone())
        .trusted_upload_metadata(trusted_upload_metadata)
        .build();

    // A validation failure anywhere from admission on — the locale lock
    // included — is the dry-run's answer, not an error.
    as_outcome(dry_run(ctx, vctx, input))
}

/// Admit the input, then judge it on the caller's connection (conn mode) or in
/// a rolled-back transaction (pool mode).
fn dry_run(
    ctx: &ServiceContext<'_>,
    vctx: &ValidateContext<'_>,
    mut input: WriteInput<'_>,
) -> Result<(), ServiceError> {
    let pending_draft = admit(ctx, vctx, &mut input)?;
    let admitted = Admitted::new(input, pending_draft);

    let Some(wh) = ctx.write_hooks else {
        return judge_rolled_back(ctx, vctx, admitted);
    };

    let conn = ctx.resolve_conn()?;

    judge(ctx, DryRun::new(wh, conn.as_ref()), vctx, admitted)
}

/// Pool mode: judge inside a transaction that is always rolled back, with the
/// runner's write hooks bound to it.
fn judge_rolled_back(
    ctx: &ServiceContext<'_>,
    vctx: &ValidateContext<'_>,
    admitted: Admitted<'_>,
) -> Result<(), ServiceError> {
    let pool = ctx.pool.context("pool required")?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start validation transaction")?;

    let mut wh = RunnerWriteHooks::new(ctx.runner()?).with_conn(&tx);
    if ctx.override_access {
        wh = wh.with_override_access();
    }

    let out = judge(ctx, DryRun::new(&wh, &tx), vctx, admitted);

    // Always roll back — this is validation only.
    drop(tx);

    out
}

/// The write's admission prefix for the previewed operation — the same
/// function the real create / update / global update calls, so the dry-run
/// judges the input that write would judge.
fn admit(
    ctx: &ServiceContext<'_>,
    vctx: &ValidateContext<'_>,
    input: &mut WriteInput<'_>,
) -> Result<PendingDraft, ServiceError> {
    if let Def::Global(def) = &ctx.def {
        return admit_global_update_input(ctx, def, input);
    }

    let def = ctx.collection_def()?;

    let Some(id) = vctx.exclude_id else {
        admit_create_input(def, input)?;

        return Ok(PendingDraft::default());
    };

    admit_update_input(ctx, def, id, input)
}

/// Everything after admission: the access gate, the stored row the field
/// rules judge, the publishing snapshot the completeness gate reads, and the
/// before-write pipeline itself.
fn judge(
    ctx: &ServiceContext<'_>,
    run: DryRun<'_>,
    vctx: &ValidateContext<'_>,
    admitted: Admitted<'_>,
) -> Result<(), ServiceError> {
    let Admitted {
        input,
        pending_draft,
    } = admitted;

    check_validate_access(ctx, run.write_hooks, vctx, &input.data, input.locale_ctx)?;

    let stored = stored_for_update_rules(ctx, vctx, run.conn, input.locale_ctx)?;

    let empty = DocumentFields::default();
    let overlay = pending_draft.publishing_snapshot(
        ctx,
        run.write_hooks,
        stored.as_ref().unwrap_or(&empty),
        input.locale_ctx,
    )?;

    let vctx = ValidateContext {
        stored_document: stored.as_ref(),
        locale_overlay: overlay.as_ref().and_then(Value::as_object),
        ..*vctx
    };

    validate_document(run.conn, run.write_hooks, &vctx, input, ctx.user)
}

/// The stored document update-mode field rules judge, read on the dry-run's
/// own connection after its access gate. `None` in create mode.
fn stored_for_update_rules(
    ctx: &ServiceContext<'_>,
    vctx: &ValidateContext<'_>,
    conn: &dyn DbConnection,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<DocumentFields>, ServiceError> {
    if vctx.operation != "update" {
        return Ok(None);
    }

    if let Def::Global(def) = &ctx.def {
        return stored_global_fields_for_update_rules(conn, ctx.slug, def, locale_ctx).map(Some);
    }

    let Some(id) = vctx.exclude_id else {
        return Ok(None);
    };

    stored_fields_for_update_rules(conn, ctx.slug, ctx.collection_def()?, id, locale_ctx).map(Some)
}

/// Enforce the target operation's collection-level access rule on the
/// dry-run — by calling the SAME gate functions the real writes call
/// (`check_create_access` / `check_update_access` /
/// `check_global_update_access`), so validate and write cannot drift on who
/// may do what. `Denied` rejects before any validator (or unique-collision
/// probe) runs.
fn check_validate_access(
    ctx: &ServiceContext<'_>,
    wh: &dyn WriteHooks,
    vctx: &ValidateContext<'_>,
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> Result<(), ServiceError> {
    let locale = locale_ctx.map(LocaleContext::access_locale);
    let ui_locale = ctx.ui_locale.as_deref();

    if let Def::Global(def) = &ctx.def {
        return check_global_update_access(ctx, wh, def, Some(data), locale, ui_locale);
    }

    let def = ctx.collection_def()?;

    match vctx.exclude_id {
        Some(id) => check_update_access(ctx, wh, def, id, data, locale, ui_locale),
        None => check_create_access(ctx, wh, def, data, locale, ui_locale),
    }
}

/// Split the dry-run result: a validation failure is a NORMAL outcome (the
/// typed error), anything else propagates.
fn as_outcome(result: Result<(), ServiceError>) -> Result<ValidateOutput, ServiceError> {
    match result {
        Ok(()) => Ok(None),
        Err(ServiceError::Validation(ve)) => Ok(Some(ve)),
        Err(e) => Err(e),
    }
}

/// Dry-run validation for a collection document.
pub enum Validate {}

impl Operation for Validate {
    type Args = ValidateArgs;
    type Output = ValidateOutput;

    const NAME: &'static str = "validate";

    // Pool mode acquires its own (rolled-back) transaction; conn mode uses
    // the Lua caller's connection — the entry's read checkout is never used.
    const READS_VIA_CONTEXT: bool = false;

    fn run(ctx: &ServiceContext<'_>, mut args: Self::Args) -> Result<Self::Output, ServiceError> {
        let def = ctx.collection_def()?;

        // Detached from `args` so the context may borrow it while the
        // remaining args move into the shared body.
        let exclude_id = args.exclude_id.take();

        let vctx = ValidateContext {
            slug: ctx.slug,
            table_name: ctx.slug,
            fields: &def.fields,
            hooks: &def.hooks,
            operation: if exclude_id.is_some() {
                "update"
            } else {
                "create"
            },
            exclude_id: exclude_id.as_deref(),
            soft_delete: def.soft_delete,
            supports_drafts: def.has_drafts(),
            required_locales: def.required_locales.as_ref(),
            // Loaded inside the body, on the dry-run's own connection.
            stored_document: None,
            // Set inside the body from the admitted pending draft.
            locale_overlay: None,
        };

        run_validate(ctx, &vctx, args)
    }
}

/// Dry-run validation for a global — always an update against the singleton
/// `default` row of `_global_<slug>`.
pub enum ValidateGlobal {}

impl Operation for ValidateGlobal {
    type Args = ValidateArgs;
    type Output = ValidateOutput;

    const NAME: &'static str = "validate_global";

    const READS_VIA_CONTEXT: bool = false;

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let def = ctx.global_def()?;
        let table = global_table(ctx.slug);

        let vctx = ValidateContext {
            slug: ctx.slug,
            table_name: &table,
            fields: &def.fields,
            hooks: &def.hooks,
            operation: "update",
            exclude_id: Some("default"),
            soft_delete: false,
            supports_drafts: def.has_drafts(),
            // Globals have no collection-level `required_locales` default.
            required_locales: None,
            stored_document: None,
            locale_overlay: None,
        };

        run_validate(ctx, &vctx, args)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result as AnyResult;
    use rusqlite::Connection;

    use super::*;
    use crate::{
        core::{CollectionDefinition, FieldDefinition, FieldType, Hooks},
        db::{AccessResult, DbConnection},
        hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
        service::{FieldReadStrip, ServiceContext},
    };

    /// Write hooks whose access check returns a fixed result.
    struct FixedAccessHooks(AccessResult);

    impl WriteHooks for FixedAccessHooks {
        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
            Ok(self.0.clone())
        }

        fn validate_fields(
            &self,
            _fields: &[FieldDefinition],
            _data: &DocumentFields,
            _ctx: &ValidationCtx,
        ) -> std::result::Result<(), crate::core::ValidationError> {
            Ok(())
        }
    }

    impl FieldReadStrip for FixedAccessHooks {}

    fn posts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.access.create = Some("acc.gate".into());
        def.access.update = Some("acc.gate".into());
        def
    }

    fn run_with(
        access: AccessResult,
        exclude_id: Option<&str>,
    ) -> Result<ValidateOutput, ServiceError> {
        let conn = Connection::open_in_memory().unwrap();
        let def = posts_def();
        let wh = FixedAccessHooks(access);
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&wh)
            .build();

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), serde_json::json!("x"));

        let args = ValidateArgs::builder(data)
            .exclude_id(exclude_id.map(ToString::to_string))
            .build();

        Validate::run(&ctx, args)
    }

    /// Regression: the dry-run is gated by the target op's collection access
    /// rule — a denied caller must not reach the validators (whose unique
    /// checks are an enumeration probe).
    #[test]
    fn validate_denied_by_access_rule() {
        let err = run_with(AccessResult::Denied, None).unwrap_err();
        assert!(
            matches!(&err, ServiceError::AccessDenied(msg) if msg.contains("Create")),
            "create-mode denial, got {err:?}"
        );

        let err = run_with(AccessResult::Denied, Some("abc")).unwrap_err();
        assert!(
            matches!(&err, ServiceError::AccessDenied(msg) if msg.contains("Update")),
            "update-mode denial, got {err:?}"
        );
    }

    #[test]
    fn validate_allowed_runs_pipeline() {
        let out = run_with(AccessResult::Allowed, None).expect("allowed validate runs");
        assert!(out.is_none(), "trivial data validates clean");
    }

    /// Constrained mirrors the write: rejected in create mode (no target row).
    #[test]
    fn validate_create_mode_rejects_constrained() {
        let err = run_with(AccessResult::Constrained(Vec::new()), None).unwrap_err();
        assert!(
            matches!(&err, ServiceError::HookError(msg) if msg.contains("filter table")),
            "got {err:?}"
        );
    }

    /// The dry-run judges the input the real write judges: both run the same
    /// admission prefix, so a pending draft, the locale lock and the upload
    /// metadata strip reach the dry-run exactly as they reach the write.
    #[cfg(feature = "sqlite")]
    mod admission {
        use std::collections::HashSet;

        use mlua::Lua;
        use serde_json::json;

        use super::*;
        use crate::{
            config::LocaleConfig,
            core::{GlobalDefinition, Registry, VersionsConfig, upload::CollectionUpload},
            db::{LocaleMode, query},
            service::{LuaWriteHooks, create_document_in_conn, update_document_in_conn},
        };

        /// Validation-only write hooks: no Lua hooks run and every access rule
        /// allows, so the outcome is the validators' alone. The registry carries
        /// no richtext nodes; leaking it gives it the hooks' lifetime.
        fn hooks(lua: &Lua) -> LuaWriteHooks<'_> {
            let registry: &'static Registry = Box::leak(Box::new(Registry::new()));

            LuaWriteHooks::builder(lua, registry)
                .override_access(true)
                .hooks_enabled(false)
                .build()
        }

        fn data(pairs: &[(&str, Value)]) -> DocumentFields {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect()
        }

        fn de() -> LocaleContext {
            LocaleContext {
                mode: LocaleMode::Single("de".to_string()),
                config: LocaleConfig {
                    default_locale: "en".to_string(),
                    locales: vec!["en".to_string(), "de".to_string()],
                    fallback: true,
                },
            }
        }

        /// The field names a dry-run outcome reports as failing.
        fn failing(out: &ValidateOutput) -> HashSet<String> {
            out.as_ref()
                .map(|ve| ve.to_field_map().into_keys().collect())
                .unwrap_or_default()
        }

        /// Whether a real write was rejected with a validation error on `field`.
        fn rejects(err: Option<ServiceError>, field: &str) -> bool {
            matches!(
                err,
                Some(ServiceError::Validation(ve)) if ve.to_field_map().contains_key(field)
            )
        }

        /// A draft-enabled `posts` collection whose `title` is required.
        fn drafted_def() -> CollectionDefinition {
            let mut def = CollectionDefinition::new("posts");
            def.versions = Some(VersionsConfig::new(true, 10));
            def.fields = vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("body", FieldType::Text).build(),
            ];

            def
        }

        /// A published `p1` whose pending draft blanked the required `title`.
        fn drafted_posts() -> Connection {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE posts (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    body TEXT,
                    _status TEXT DEFAULT 'published',
                    created_at TEXT,
                    updated_at TEXT
                );
                CREATE TABLE _versions_posts (
                    id TEXT PRIMARY KEY,
                    _parent TEXT,
                    _version INTEGER,
                    _status TEXT,
                    _latest INTEGER DEFAULT 0,
                    snapshot TEXT,
                    created_at TEXT
                );
                INSERT INTO posts (id, title) VALUES ('p1', 'Published');",
            )
            .unwrap();

            let drafted = json!({ "title": "", "body": "drafted" });
            query::create_version(&conn, "posts", "p1", "draft", &drafted).unwrap();

            conn
        }

        /// Regression: a publish dry-run that omits a field the pending draft
        /// blanked reported `valid`, while the publish itself — which adopts the
        /// draft as its base — was rejected on that field.
        #[test]
        fn a_publish_dry_run_judges_the_pending_draft() {
            let conn = drafted_posts();
            let def = drafted_def();
            let lua = Lua::new();
            let wh = hooks(&lua);
            let ctx = ServiceContext::collection("posts", &def)
                .conn(&conn)
                .write_hooks(&wh)
                .build();

            let patch = || data(&[("body", json!("edited"))]);

            let out = Validate::run(
                &ctx,
                ValidateArgs::builder(patch())
                    .exclude_id(Some("p1".to_string()))
                    .build(),
            )
            .unwrap();
            assert!(failing(&out).contains("title"), "got {out:?}");

            let real = update_document_in_conn(&ctx, "p1", WriteInput::builder(patch()).build());
            assert!(
                rejects(real.err(), "title"),
                "the publish it previews is rejected"
            );

            let draft = Validate::run(
                &ctx,
                ValidateArgs::builder(patch())
                    .exclude_id(Some("p1".to_string()))
                    .draft(true)
                    .build(),
            )
            .unwrap();
            assert!(draft.is_none(), "a draft dry-run adopts nothing: {draft:?}");
        }

        /// Regression: a non-default-locale dry-run carrying a shared field
        /// reported `valid`, while the write rejected it through the locale lock.
        #[test]
        fn a_translation_dry_run_applies_the_locale_lock() {
            let conn = Connection::open_in_memory().unwrap();
            let mut def = CollectionDefinition::new("posts");
            def.fields = vec![
                FieldDefinition::builder("slug", FieldType::Text).build(),
                FieldDefinition::builder("title", FieldType::Text)
                    .localized(true)
                    .build(),
            ];
            let lua = Lua::new();
            let wh = hooks(&lua);
            let ctx = ServiceContext::collection("posts", &def)
                .conn(&conn)
                .write_hooks(&wh)
                .build();

            let patch = || data(&[("slug", json!("neu"))]);

            let out = Validate::run(
                &ctx,
                ValidateArgs::builder(patch())
                    .locale_ctx(Some(de()))
                    .exclude_id(Some("p1".to_string()))
                    .build(),
            )
            .unwrap();
            assert!(failing(&out).contains("slug"), "got {out:?}");

            let locale = de();
            let real = update_document_in_conn(
                &ctx,
                "p1",
                WriteInput::builder(patch())
                    .locale_ctx(Some(&locale))
                    .build(),
            );
            assert!(
                rejects(real.err(), "slug"),
                "the write it previews is rejected"
            );
        }

        /// Regression: a caller-supplied `filename` on an upload collection
        /// satisfied `required` in the dry-run, while the write strips it as
        /// server-derived metadata and fails. The admin multipart preview, whose
        /// real write is trusted, keeps its metadata.
        #[test]
        fn an_upload_dry_run_strips_caller_supplied_metadata() {
            let conn = Connection::open_in_memory().unwrap();
            let mut def = CollectionDefinition::new("media");
            def.upload = Some(CollectionUpload::new());
            def.fields = vec![
                FieldDefinition::builder("filename", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("caption", FieldType::Text).build(),
            ];
            let lua = Lua::new();
            let wh = hooks(&lua);
            let ctx = ServiceContext::collection("media", &def)
                .conn(&conn)
                .write_hooks(&wh)
                .build();

            let forged = || data(&[("filename", json!("forged.jpg")), ("caption", json!("hi"))]);

            let out = Validate::run(&ctx, ValidateArgs::builder(forged()).build()).unwrap();
            assert!(failing(&out).contains("filename"), "got {out:?}");

            let real = create_document_in_conn(&ctx, WriteInput::builder(forged()).build());
            assert!(
                rejects(real.err(), "filename"),
                "the write it previews is rejected"
            );

            let trusted = Validate::run(
                &ctx,
                ValidateArgs::builder(forged())
                    .trusted_upload_metadata(true)
                    .build(),
            )
            .unwrap();
            assert!(trusted.is_none(), "got {trusted:?}");
        }

        /// A draft-enabled `settings` global whose `a` is required, with a pending
        /// draft that blanked it.
        fn drafted_settings() -> (GlobalDefinition, Connection) {
            let mut def = GlobalDefinition::new("settings");
            def.versions = Some(VersionsConfig::new(true, 10));
            def.fields = vec![
                FieldDefinition::builder("a", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("b", FieldType::Text).build(),
            ];

            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE _versions__global_settings (
                    id TEXT PRIMARY KEY,
                    _parent TEXT NOT NULL,
                    _version INTEGER NOT NULL,
                    _status TEXT NOT NULL,
                    _latest INTEGER NOT NULL DEFAULT 0,
                    snapshot TEXT NOT NULL,
                    created_at TEXT
                );",
            )
            .unwrap();

            let drafted = json!({ "a": "", "b": "drafted b" });
            query::create_version(&conn, "_global_settings", "default", "draft", &drafted).unwrap();

            (def, conn)
        }

        /// The global twin of the pending-draft regression.
        #[test]
        fn a_global_publish_dry_run_judges_the_pending_draft() {
            let (def, conn) = drafted_settings();
            let lua = Lua::new();
            let wh = hooks(&lua);
            let ctx = ServiceContext::global("settings", &def)
                .conn(&conn)
                .write_hooks(&wh)
                .build();

            let patch = || data(&[("b", json!("edited"))]);

            let out = ValidateGlobal::run(&ctx, ValidateArgs::builder(patch()).build()).unwrap();
            assert!(failing(&out).contains("a"), "got {out:?}");

            let draft =
                ValidateGlobal::run(&ctx, ValidateArgs::builder(patch()).draft(true).build())
                    .unwrap();
            assert!(draft.is_none(), "a draft dry-run adopts nothing: {draft:?}");
        }

        /// The global twin of the locale-lock regression.
        #[test]
        fn a_global_translation_dry_run_applies_the_locale_lock() {
            let conn = Connection::open_in_memory().unwrap();
            let mut def = GlobalDefinition::new("settings");
            def.fields = vec![FieldDefinition::builder("slug", FieldType::Text).build()];
            let lua = Lua::new();
            let wh = hooks(&lua);
            let ctx = ServiceContext::global("settings", &def)
                .conn(&conn)
                .write_hooks(&wh)
                .build();

            let out = ValidateGlobal::run(
                &ctx,
                ValidateArgs::builder(data(&[("slug", json!("neu"))]))
                    .locale_ctx(Some(de()))
                    .build(),
            )
            .unwrap();

            assert!(failing(&out).contains("slug"), "got {out:?}");
        }
    }
}
