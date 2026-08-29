//! The `delete` operation (soft or permanent).

use crate::{
    core::{CollectionDefinition, ReqContext},
    service::{ServiceContext, ServiceError, delete_document},
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`Delete`].
#[derive(Builder)]
pub struct DeleteArgs {
    #[builder(required)]
    pub id: String,
    /// Permanently delete even when the collection has soft delete. Expressed
    /// by disabling soft-delete on a local definition clone (see
    /// [`Operation::adjust_collection_def`]) — previously copy-pasted on
    /// every surface.
    pub force_hard_delete: bool,
    /// Publish a mutation event for this write (request `events` flag).
    #[builder(default = true)]
    pub events: bool,
}

/// Delete a document: soft delete when the collection has it (gated by
/// `access.trash ?? update`), permanent otherwise (gated by
/// `access.delete`). Upload files are cleaned via the context's storage.
pub enum Delete {}

impl Operation for Delete {
    type Args = DeleteArgs;
    type Output = ReqContext;

    const NAME: &'static str = "delete";

    const READS_VIA_CONTEXT: bool = false;

    fn emit_events(args: &Self::Args) -> bool {
        args.events
    }

    fn adjust_collection_def(
        args: &Self::Args,
        def: &CollectionDefinition,
    ) -> Option<CollectionDefinition> {
        (args.force_hard_delete && def.soft_delete).then(|| {
            let mut d = def.clone();
            d.make_hard_delete();
            d
        })
    }

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        delete_document(ctx, &args.id, ctx.storage.as_deref(), ctx.locale_config)
    }
}
