//! The `validate` (dry-run) operations — collection and global.
//!
//! Runs the full before-write pipeline (field-access stripping, field hooks,
//! validators, unique checks, `before_validate` hooks) without persisting and
//! returns the typed outcome (`None` = valid, `Some(ValidationError)` = the
//! per-field failures).
//!
//! Access semantics match the surface's REAL write: the acting user (or MCP's
//! override) drives field-level write-access stripping, so a dry-run predicts
//! exactly what the corresponding create/update would do. (MCP validate
//! previously ran as anonymous WITHOUT override, so its dry-run could report
//! field strips the actual override write would never apply.)

use anyhow::Context as _;

use crate::{
    core::{DocumentFields, ValidationError},
    db::{LocaleContext, query::helpers::global_table},
    service::{
        RunnerWriteHooks, ServiceContext, ServiceError, ValidateContext, WriteInput,
        validate_document,
    },
};

use super::Operation;

/// Owned arguments for [`Validate`] / [`ValidateGlobal`].
pub struct ValidateArgs {
    pub data: DocumentFields,
    pub locale_ctx: Option<LocaleContext>,
    /// Update-mode target id, excluded from unique checks. `None` = create
    /// mode. Ignored by [`ValidateGlobal`] (always update against `default`).
    pub exclude_id: Option<String>,
    /// Validate as a draft write (skips required-field checks where the
    /// target supports drafts — the body clamps, like the real write path).
    pub draft: bool,
}

impl ValidateArgs {
    #[must_use]
    pub fn builder(data: DocumentFields) -> ValidateArgsBuilder {
        ValidateArgsBuilder {
            data,
            locale_ctx: None,
            exclude_id: None,
            draft: false,
        }
    }
}

/// Builder for [`ValidateArgs`].
pub struct ValidateArgsBuilder {
    data: DocumentFields,
    locale_ctx: Option<LocaleContext>,
    exclude_id: Option<String>,
    draft: bool,
}

impl ValidateArgsBuilder {
    #[must_use]
    pub fn locale_ctx(mut self, locale_ctx: Option<LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    #[must_use]
    pub fn exclude_id(mut self, exclude_id: Option<String>) -> Self {
        self.exclude_id = exclude_id;
        self
    }

    #[must_use]
    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;
        self
    }

    #[must_use]
    pub fn build(self) -> ValidateArgs {
        ValidateArgs {
            data: self.data,
            locale_ctx: self.locale_ctx,
            exclude_id: self.exclude_id,
            draft: self.draft,
        }
    }
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
    let input = WriteInput::builder(args.data)
        .locale_ctx(args.locale_ctx.as_ref())
        .draft(args.draft)
        .ui_locale(ctx.ui_locale.clone())
        .build();

    if let Some(wh) = ctx.write_hooks {
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

    let out = as_outcome(validate_document(&tx, &wh, vctx, input, ctx.user));

    // Always roll back — this is validation only.
    drop(tx);

    out
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
