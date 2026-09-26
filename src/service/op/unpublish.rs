//! The `unpublish` operation.

use crate::{
    core::{Builder, Document},
    service::{ServiceContext, ServiceError, unpublish_document},
};

use super::Operation;

/// Owned arguments for [`Unpublish`].
#[derive(Builder)]
pub struct UnpublishArgs {
    #[builder(required)]
    pub id: String,
    /// Publish a mutation event for this write (request `events` flag).
    #[builder(default = true)]
    pub events: bool,
    /// The document revision the caller last read. Set, the unpublish is
    /// refused with a conflict when the document has been written since.
    pub expected_revision: Option<i64>,
}

/// Revert a document to draft status. The service gate rejects unpublish on
/// a non-versioned collection with an explicit error on every surface.
pub enum Unpublish {}

impl Operation for Unpublish {
    type Args = UnpublishArgs;
    type Output = Document;

    const NAME: &'static str = "unpublish";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        unpublish_document(ctx, &args.id, args.expected_revision)
    }
}
