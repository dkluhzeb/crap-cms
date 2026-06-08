//! Validation context bundling DB + request parameters consumed by every check.

use crate::{
    core::{Document, RequiredLocales, registry::Registry},
    db::{DbConnection, LocaleContext},
};

/// Context for field validation, bundling database and request parameters.
pub struct ValidationCtx<'a> {
    pub conn: &'a dyn DbConnection,
    pub table: &'a str,
    pub exclude_id: Option<&'a str>,
    pub is_draft: bool,
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Registry for looking up richtext node definitions during node attr validation.
    pub registry: Option<&'a Registry>,
    /// When true, unique constraint checks exclude soft-deleted documents.
    pub soft_delete: bool,
    /// Collection-level `required_locales` default — the fallback for localized
    /// required fields that don't set their own (used by the completeness check).
    pub collection_required_locales: Option<&'a RequiredLocales>,
    /// The authenticated user, exposed to custom `validate` functions as
    /// `ctx.user` (via VM app-data). `None` when unauthenticated.
    pub user: Option<&'a Document>,
    /// The admin UI locale, exposed to custom `validate` functions as
    /// `ctx.ui_locale`.
    pub ui_locale: Option<&'a str>,
}

impl<'a> ValidationCtx<'a> {
    /// Create a builder with the required connection and table name.
    pub fn builder(conn: &'a dyn DbConnection, table: &'a str) -> ValidationCtxBuilder<'a> {
        ValidationCtxBuilder::new(conn, table)
    }
}

/// Builder for [`ValidationCtx`]. Created via [`ValidationCtx::builder`].
pub struct ValidationCtxBuilder<'a> {
    conn: &'a dyn DbConnection,
    table: &'a str,
    exclude_id: Option<&'a str>,
    is_draft: bool,
    locale_ctx: Option<&'a LocaleContext>,
    registry: Option<&'a Registry>,
    soft_delete: bool,
    collection_required_locales: Option<&'a RequiredLocales>,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

impl<'a> ValidationCtxBuilder<'a> {
    fn new(conn: &'a dyn DbConnection, table: &'a str) -> Self {
        Self {
            conn,
            table,
            exclude_id: None,
            is_draft: false,
            locale_ctx: None,
            registry: None,
            soft_delete: false,
            collection_required_locales: None,
            user: None,
            ui_locale: None,
        }
    }

    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<&'a str>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    /// Set the collection-level `required_locales` default.
    pub fn collection_required_locales(mut self, v: Option<&'a RequiredLocales>) -> Self {
        self.collection_required_locales = v;
        self
    }

    pub fn exclude_id(mut self, exclude_id: Option<&'a str>) -> Self {
        self.exclude_id = exclude_id;
        self
    }

    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn registry(mut self, registry: &'a Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn soft_delete(mut self, soft_delete: bool) -> Self {
        self.soft_delete = soft_delete;
        self
    }

    pub fn build(self) -> ValidationCtx<'a> {
        ValidationCtx {
            conn: self.conn,
            table: self.table,
            exclude_id: self.exclude_id,
            is_draft: self.is_draft,
            locale_ctx: self.locale_ctx,
            registry: self.registry,
            soft_delete: self.soft_delete,
            collection_required_locales: self.collection_required_locales,
            user: self.user,
            ui_locale: self.ui_locale,
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    #[test]
    fn builder_defaults_to_no_exclusions_and_false_flags() {
        let conn = InMemoryConn::open();
        let ctx = ValidationCtx::builder(&conn, "posts").build();

        assert_eq!(ctx.table, "posts");
        assert!(ctx.exclude_id.is_none());
        assert!(!ctx.is_draft);
        assert!(!ctx.soft_delete);
        assert!(ctx.locale_ctx.is_none());
        assert!(ctx.registry.is_none());
    }

    /// `is_draft` and `soft_delete` are both `bool` — distinct values catch a
    /// swapped assignment in `build()` that would silently change query scoping.
    #[test]
    fn builder_wires_each_field_to_its_own_slot() {
        let conn = InMemoryConn::open();
        let registry = Registry::new();

        let ctx = ValidationCtx::builder(&conn, "posts")
            .exclude_id(Some("doc-7"))
            .draft(true)
            .soft_delete(false)
            .registry(&registry)
            .build();

        assert_eq!(ctx.exclude_id, Some("doc-7"));
        assert!(ctx.is_draft);
        assert!(!ctx.soft_delete);
        assert!(ctx.registry.is_some());
    }
}
