//! The `create_many` bulk operation.

use crate::service::{
    CreateManyItem, CreateManyOptions, CreateManyResult, ServiceContext, ServiceError, create_many,
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`CreateMany`]. Items carry per-item policed
/// passwords (auth seeding); the service chokepoint validates and hashes.
#[derive(Builder)]
pub struct CreateManyArgs {
    #[builder(required)]
    pub items: Vec<CreateManyItem>,
    #[builder(default = true)]
    pub run_hooks: bool,
    pub draft: bool,
    /// `server.bulk_max_documents` cap. `0` = no limit.
    pub max_documents: i64,
    /// Publish mutation events (bulk surfaces default this to `false`).
    pub events: bool,
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
