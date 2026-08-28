//! The `undelete` (restore-from-trash) operation.

use crate::{
    core::Document,
    service::{ServiceContext, ServiceError, undelete_document},
};

use super::Operation;

/// Owned arguments for [`Undelete`].
pub struct UndeleteArgs {
    pub id: String,
    /// Publish a mutation event for this write (request `events` flag).
    pub events: bool,
}

impl UndeleteArgs {
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

/// Restore a soft-deleted document. Soft-delete support and trash gating are
/// enforced at the service chokepoint, so every surface agrees.
pub enum Undelete {}

impl Operation for Undelete {
    type Args = UndeleteArgs;
    type Output = Document;

    const NAME: &'static str = "undelete";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        undelete_document(ctx, &args.id)
    }
}
