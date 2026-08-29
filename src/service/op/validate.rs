//! The `validate` (dry-run) operations — collection and global.
//!
//! Runs the full before-write pipeline (field-access stripping, field hooks,
//! validators, unique checks, `before_validate` hooks) without persisting and
//! returns the typed outcome (`None` = valid, `Some(ValidationError)` = the
//! per-field failures).
//!
//! Access semantics match the surface's REAL write: the target operation's
//! collection-level access rule (`access.create` / `access.update`) gates the
//! dry-run exactly like the write it previews — an anonymous caller denied
//! the write is denied the dry-run too, closing the unique-collision
//! enumeration channel an ungated validate offered — and the acting user (or
//! MCP's override) drives field-level write-access stripping. (MCP validate
//! previously ran as anonymous WITHOUT override, so its dry-run could report
//! field strips the actual override write would never apply.)

use anyhow::Context as _;

use crate::{
    core::{DocumentFields, ValidationError, nest_group_fields},
    db::{LocaleContext, query::helpers::global_table},
    service::{
        Def, RunnerWriteHooks, ServiceContext, ServiceError, ValidateContext, WriteInput,
        check_create_access, check_global_update_access, check_update_access, hooks::WriteHooks,
        validate_document,
    },
};

use crate::core::Builder;

use super::Operation;

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
    /// target supports drafts — the body clamps, like the real write path).
    pub draft: bool,
}

/// The dry-run outcome: `None` = valid; `Some(err)` = the typed validation
/// failure. Kept typed (not pre-flattened to a field map) so the admin codec
/// can translate messages via i18n while API codecs flatten with
/// [`ValidationError::to_field_map`]. Non-validation failures propagate as
/// `Err(ServiceError)`.
pub type ValidateOutput = Option<ValidationError>;

/// Run the dry-run against an assembled [`ValidateContext`].
///
/// **Conn mode** (`ctx.write_hooks` set — Lua inside a hook transaction):
/// runs on the caller's connection with the caller's hooks; side effects of
/// `before_validate` hooks follow the outer commit/rollback.
///
/// **Pool mode** (every other surface): runs inside a transaction that is
/// always ROLLED BACK, so hook side effects during validation are discarded.
/// Previously only the admin endpoint did this — a `before_validate` hook
/// that wrote via nested CRUD could persist from a gRPC/MCP dry-run.
fn run_validate(
    ctx: &ServiceContext<'_>,
    vctx: &ValidateContext<'_>,
    args: ValidateArgs,
) -> Result<ValidateOutput, ServiceError> {
    let ValidateArgs {
        data,
        locale_ctx,
        draft,
        exclude_id: _,
    } = args;

    // Canonicalize to nested groups BEFORE the access check — the real write
    // bodies nest first too, so an access hook reading `ctx.data.seo.title`
    // sees the same shape on the dry-run as on the write.
    // (`validate_document` nests again internally; the operation is
    // idempotent.)
    let data = nest_group_fields(&data, vctx.fields);

    if let Some(wh) = ctx.write_hooks {
        check_validate_access(ctx, wh, vctx, &data, locale_ctx.as_ref())?;

        let input = WriteInput::builder(data)
            .locale_ctx(locale_ctx.as_ref())
            .draft(draft)
            .ui_locale(ctx.ui_locale.clone())
            .build();
        let conn = ctx.resolve_conn()?;

        return as_outcome(validate_document(conn.as_ref(), wh, vctx, input, ctx.user));
    }

    let pool = ctx.pool.context("pool required")?;
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction().context("Start validation transaction")?;

    let mut wh = RunnerWriteHooks::new(ctx.runner()?).with_conn(&tx);
    if ctx.override_access {
        wh = wh.with_override_access();
    }

    check_validate_access(ctx, &wh, vctx, &data, locale_ctx.as_ref())?;

    let input = WriteInput::builder(data)
        .locale_ctx(locale_ctx.as_ref())
        .draft(draft)
        .ui_locale(ctx.ui_locale.clone())
        .build();
    let out = as_outcome(validate_document(&tx, &wh, vctx, input, ctx.user));

    // Always roll back — this is validation only.
    drop(tx);

    out
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
        service::ServiceContext,
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
}
