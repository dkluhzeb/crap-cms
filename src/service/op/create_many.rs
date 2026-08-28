//! The `create_many` bulk operation.

use crate::service::{
    CreateManyItem, CreateManyOptions, CreateManyResult, ServiceContext, ServiceError, create_many,
};

use super::Operation;

/// Owned arguments for [`CreateMany`]. Items carry per-item policed
/// passwords (auth seeding); the service chokepoint validates and hashes.
pub struct CreateManyArgs {
    pub items: Vec<CreateManyItem>,
    pub run_hooks: bool,
    pub draft: bool,
    /// `server.bulk_max_documents` cap. `0` = no limit.
    pub max_documents: i64,
    /// Publish mutation events (bulk surfaces default this to `false`).
    pub events: bool,
}

impl CreateManyArgs {
    #[must_use]
    pub fn builder(items: Vec<CreateManyItem>) -> CreateManyArgsBuilder {
        CreateManyArgsBuilder::new(items)
    }
}

/// Builder for [`CreateManyArgs`].
pub struct CreateManyArgsBuilder {
    items: Vec<CreateManyItem>,
    run_hooks: bool,
    draft: bool,
    max_documents: i64,
    events: bool,
}

impl CreateManyArgsBuilder {
    fn new(items: Vec<CreateManyItem>) -> Self {
        Self {
            items,
            run_hooks: true,
            draft: false,
            max_documents: 0,
            events: false,
        }
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
    pub fn build(self) -> CreateManyArgs {
        CreateManyArgs {
            items: self.items,
            run_hooks: self.run_hooks,
            draft: self.draft,
            max_documents: self.max_documents,
            events: self.events,
        }
    }
}

/// Bulk-create documents in one transaction.
pub enum CreateMany {}

impl Operation for CreateMany {
    type Args = CreateManyArgs;
    type Output = CreateManyResult;

    const NAME: &'static str = "create_many";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let opts = CreateManyOptions {
            run_hooks: args.run_hooks,
            draft: args.draft,
            max_documents: args.max_documents,
        };

        create_many(ctx, &args.items, &opts)
    }
}
