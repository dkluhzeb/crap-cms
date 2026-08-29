//! Input for `find_document_by_id` — single document lookup.
//!
//! Carries only genuine per-call data. Infrastructure (registry, populate
//! cache, singleflight) lives on the `ServiceContext` — set in one shot by
//! `ServiceContextBuilder::infra` — so an input can never smuggle a stale or
//! missing infra dependency past the context.

use crate::{core::Builder, db::LocaleContext, service::read::post_process::PostProcessOpts};

/// Input for [`find_document_by_id`](crate::service::find_document_by_id).
#[derive(Builder)]
pub struct FindByIdInput<'a> {
    #[builder(required)]
    pub id: &'a str,
    pub depth: i32,
    pub select: Option<&'a [String]>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub use_draft: bool,
    /// When true, include soft-deleted documents (trash view).
    pub include_deleted: bool,
}

impl PostProcessOpts for FindByIdInput<'_> {
    fn depth(&self) -> i32 {
        self.depth
    }
    fn include_drafts(&self) -> bool {
        // `use_draft` means the caller asked for the draft overlay — i.e.
        // an editor previewing — so draft relationship targets are visible.
        // A normal (published) read hides them.
        self.use_draft
    }
    fn hydrate(&self) -> bool {
        false
    }
    fn select(&self) -> Option<&[String]> {
        self.select
    }
    fn locale_ctx(&self) -> Option<&LocaleContext> {
        self.locale_ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SAFE-DEFAULT GUARD: by default a by-id read must not surface a draft
    /// overlay or a soft-deleted row. Flipping these defaults would leak
    /// unpublished/trashed documents on every surface that uses the builder.
    #[test]
    fn builder_defaults_are_restrictive() {
        let input = FindByIdInput::builder("id").build();
        assert!(!input.use_draft, "draft overlay must be off by default");
        assert!(
            !input.include_deleted,
            "soft-deleted rows excluded by default"
        );
    }
}
