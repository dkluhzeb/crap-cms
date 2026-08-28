//! The `unpublish` operation.

use crate::{
    core::Document,
    service::{ServiceContext, ServiceError, unpublish_document},
};

use super::Operation;

/// Owned arguments for [`Unpublish`].
pub struct UnpublishArgs {
    pub id: String,
    /// Publish a mutation event for this write (request `events` flag).
    pub events: bool,
}

impl UnpublishArgs {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            events: true,
        }
    }

    #[must_use]
    pub fn events(mut self, events: bool) -> Self {
        self.events = events;
        self
    }
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
        unpublish_document(ctx, &args.id)
    }
}
