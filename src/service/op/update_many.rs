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

use super::Operation;

/// Owned arguments for [`UpdateMany`]. A `password` never travels here — a
/// broadcast write must not set one credential on many rows; every surface
/// rejects it at decode.
pub struct UpdateManyArgs {
    pub filters: Vec<FilterClause>,
    pub data: DocumentFields,
    pub locale_ctx: Option<LocaleContext>,
    pub run_hooks: bool,
    pub draft: bool,
    /// `server.bulk_max_documents` cap. `0` = no limit.
    pub max_documents: i64,
    /// Publish mutation events (bulk surfaces default this to `false`).
    pub events: bool,
}

impl UpdateManyArgs {
    #[must_use]
    pub fn builder(filters: Vec<FilterClause>, data: DocumentFields) -> UpdateManyArgsBuilder {
        UpdateManyArgsBuilder::new(filters, data)
    }
}

/// Builder for [`UpdateManyArgs`].
pub struct UpdateManyArgsBuilder {
    filters: Vec<FilterClause>,
    data: DocumentFields,
    locale_ctx: Option<LocaleContext>,
    run_hooks: bool,
    draft: bool,
    max_documents: i64,
    events: bool,
}

impl UpdateManyArgsBuilder {
    fn new(filters: Vec<FilterClause>, data: DocumentFields) -> Self {
        Self {
            filters,
            data,
            locale_ctx: None,
            run_hooks: true,
            draft: false,
            max_documents: 0,
            events: false,
        }
    }

    #[must_use]
    pub fn locale_ctx(mut self, locale_ctx: Option<LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    #[must_use]
    pub fn run_hooks(mut self, run_hooks: bool) -> Self {
        self.run_hooks = run_hooks;
        self
    }

    #[must_use]
    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;
        self
    }

    #[must_use]
    pub fn max_documents(mut self, max_documents: i64) -> Self {
        self.max_documents = max_documents;
        self
    }

    #[must_use]
    pub fn events(mut self, events: bool) -> Self {
        self.events = events;
        self
    }

    #[must_use]
    pub fn build(self) -> UpdateManyArgs {
        UpdateManyArgs {
            filters: self.filters,
            data: self.data,
            locale_ctx: self.locale_ctx,
            run_hooks: self.run_hooks,
            draft: self.draft,
            max_documents: self.max_documents,
            events: self.events,
        }
    }
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
