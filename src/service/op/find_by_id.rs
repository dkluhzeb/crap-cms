//! The `find_by_id` operation — the Stage-2 reference conversion.

use crate::{
    core::Document,
    db::LocaleContext,
    service::{FindByIdInput, ServiceContext, ServiceError, find_document_by_id},
};

use crate::core::Builder;

use super::Operation;

/// Owned arguments for [`FindById`]. Codecs decode their wire format into
/// this and pass the flags RAW — the definition-dependent downgrades (draft
/// needs versions, trash needs soft-delete) happen once in [`FindById::run`],
/// so no surface can drift on them.
#[derive(Builder)]
pub struct FindByIdArgs {
    #[builder(required)]
    pub id: String,
    pub depth: i32,
    pub select: Option<Vec<String>>,
    pub locale_ctx: Option<LocaleContext>,
    /// Overlay the latest draft snapshot (editors previewing). Ignored unless
    /// the collection has versions + drafts.
    pub use_draft: bool,
    /// Read from the trash view (soft-deleted rows). Ignored unless the
    /// collection has soft delete.
    pub include_deleted: bool,
}

/// Single-document lookup by ID. See [`find_document_by_id`] for the
/// lifecycle (access → `before_read` → SELECT → post-process).
pub enum FindById {}

impl Operation for FindById {
    type Args = FindByIdArgs;
    type Output = Option<Document>;

    const NAME: &'static str = "find_by_id";

    fn run(ctx: &ServiceContext<'_>, args: Self::Args) -> Result<Self::Output, ServiceError> {
        let def = ctx.collection_def()?;

        // Definition-dependent flag downgrades, harmonized here: a draft
        // overlay needs drafts + versions; a trash read needs soft delete.
        // (gRPC and the Lua/MCP `find` list paths already downgraded; the Lua
        // `find_by_id` trash flag was the outlier that passed raw.)
        let use_draft = args.use_draft && def.has_drafts() && def.has_versions();
        let include_deleted = args.include_deleted && def.soft_delete;

        let input = FindByIdInput::builder(&args.id)
            .depth(args.depth)
            .select(args.select.as_deref())
            .locale_ctx(args.locale_ctx.as_ref())
            .use_draft(use_draft)
            .include_deleted(include_deleted)
            .build();

        find_document_by_id(ctx, &input)
    }
}
