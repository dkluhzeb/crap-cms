//! The `update_many` bulk operation.

use anyhow::anyhow;

use crate::{
    core::DocumentFields,
    db::{FilterClause, LocaleContext, query::filter::normalize_filter_fields},
    service::{
        ServiceContext, ServiceError, UpdateManyOptions, UpdateManyResult, update_many,
        validate_user_filters,
    },
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`UpdateMany`]. A `password` never travels here — a
/// broadcast write must not set one credential on many rows; every surface
/// rejects it at decode.
#[derive(Builder)]
pub struct UpdateManyArgs {
    #[builder(required)]
    pub filters: Vec<FilterClause>,
    #[builder(required)]
    pub data: DocumentFields,
    pub locale_ctx: Option<LocaleContext>,
    #[builder(default = true)]
    pub run_hooks: bool,
    pub draft: bool,
    /// `server.bulk_max_documents` cap. `0` = no limit.
    pub max_documents: i64,
    /// Publish mutation events (bulk surfaces default this to `false`).
    pub events: bool,
}

/// Bulk-update all documents matching a filter in one transaction.
pub enum UpdateMany {}

impl Operation for UpdateMany {
    type Args = UpdateManyArgs;
    type Output = UpdateManyResult;

    const NAME: &'static str = "update_many";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let locale_config = ctx
            .locale_config
            .ok_or_else(|| ServiceError::Internal(anyhow!("update_many requires locale_config")))?;
        let def = ctx.collection_def()?;

        // Same wire-filter hygiene as `Find::run`: dotted group paths
        // normalize, user filters must not touch system columns — uniform on
        // every surface (MCP previously did neither).
        let mut filters = args.filters;
        normalize_filter_fields(&mut filters, &def.fields);
        validate_user_filters(&filters).map_err(|e| ServiceError::HookError(e.to_string()))?;

        let opts = UpdateManyOptions {
            locale_ctx: args.locale_ctx.as_ref(),
            run_hooks: args.run_hooks,
            draft: args.draft,
            ui_locale: ctx.ui_locale.clone(),
            max_documents: args.max_documents,
        };

        update_many(ctx, &filters, &args.data, locale_config, &opts)
    }
}
