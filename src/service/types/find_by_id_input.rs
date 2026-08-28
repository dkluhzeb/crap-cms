//! Input for `find_document_by_id` — single document lookup.
//!
//! Carries only genuine per-call data. Infrastructure (registry, populate
//! cache, singleflight) lives on the `ServiceContext` — set in one shot by
//! `ServiceContextBuilder::infra` — so an input can never smuggle a stale or
//! missing infra dependency past the context.

use crate::{db::LocaleContext, service::read::post_process::PostProcessOpts};

/// Input for [`find_document_by_id`](crate::service::find_document_by_id).
pub struct FindByIdInput<'a> {
    pub id: &'a str,
    pub depth: i32,
    pub select: Option<&'a [String]>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub use_draft: bool,
    /// When true, include soft-deleted documents (trash view).
    pub include_deleted: bool,
}

impl<'a> FindByIdInput<'a> {
    #[must_use]
    pub fn builder(id: &'a str) -> FindByIdInputBuilder<'a> {
        FindByIdInputBuilder::new(id)
    }
}

/// Builder for [`FindByIdInput`].
pub struct FindByIdInputBuilder<'a> {
    id: &'a str,
    depth: i32,
    select: Option<&'a [String]>,
    locale_ctx: Option<&'a LocaleContext>,
    use_draft: bool,
    include_deleted: bool,
}

impl<'a> FindByIdInputBuilder<'a> {
    pub fn new(id: &'a str) -> Self {
        Self {
            id,
            depth: 0,
            select: None,
            locale_ctx: None,
            use_draft: false,
            include_deleted: false,
        }
    }

    pub fn depth(mut self, depth: i32) -> Self {
        self.depth = depth;
        self
    }

    pub fn select(mut self, select: Option<&'a [String]>) -> Self {
        self.select = select;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn use_draft(mut self, use_draft: bool) -> Self {
        self.use_draft = use_draft;
        self
    }

    pub fn include_deleted(mut self, include_deleted: bool) -> Self {
        self.include_deleted = include_deleted;
        self
    }

    pub fn build(self) -> FindByIdInput<'a> {
        FindByIdInput {
            id: self.id,
            depth: self.depth,
            select: self.select,
            locale_ctx: self.locale_ctx,
            use_draft: self.use_draft,
            include_deleted: self.include_deleted,
        }
    }
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
    fn ui_locale(&self) -> Option<&str> {
        None
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
