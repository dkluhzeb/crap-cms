//! Validation context bundling DB + request parameters consumed by every check.

use serde_json::{Map, Value};

use crate::{
    core::{Document, DocumentFields, RequiredLocales, registry::Registry},
    db::{DbConnection, LocaleContext},
};

/// A source of the edited document's stored values a write may resubmit
/// unchanged (see [`HeldValueGate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldSource {
    /// The stored row.
    StoredRow,
    /// The pending draft kept as a version snapshot.
    PendingDraft,
}

/// Decides what of the edited document's stored values the writer may lean
/// on. A value a check would now refuse (a retired option, a rich text node
/// the field no longer enables) is accepted when the document already holds
/// it — but that acceptance says the document holds it, so it may count only
/// where the writer could read it: the source's content view, and each field's
/// read access. Implemented by the service layer, which knows the writer.
pub trait HeldValueGate {
    /// Whether the writer may see `source` at all; when it may, `fields` (that
    /// source's values) are stripped of what the writer may not read.
    fn admit(&self, source: HeldSource, fields: &mut DocumentFields) -> bool;
}

/// Context for field validation, bundling database and request parameters.
pub struct ValidationCtx<'a> {
    pub conn: &'a dyn DbConnection,
    pub table: &'a str,
    pub exclude_id: Option<&'a str>,
    pub is_draft: bool,
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Registry for looking up richtext node definitions during node attr
    /// validation. Write paths never set it by hand: `validate_write_fields`
    /// fills it from its required registry argument.
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
    /// The snapshot a later step of this same write lands over the row, in
    /// snapshot key form (`field__xx`, groups as the snapshot keeps them) — a
    /// publish's pending draft, or the version being restored.
    ///
    /// The localized-completeness gate judges the document's post-write state,
    /// so it reads the locales this request does not target from here instead
    /// of from the live row; a locale the snapshot does not carry stays as
    /// stored, which is the write-back's own rule. `None` when the write lands
    /// nothing beyond its own data.
    pub locale_overlay: Option<&'a Map<String, Value>>,
    /// Whether the edited document keeps its drafts as version snapshots
    /// (drafts and versions both enabled). A value the pending draft already
    /// holds then counts as held, like one the stored row holds — the edit
    /// form shows the draft. `false` reads the stored row alone.
    pub versioned_drafts: bool,
    /// What of the stored sources the writer may lean on; `None` leans on
    /// every source unfiltered (a caller judging no writer).
    pub held_gate: Option<&'a dyn HeldValueGate>,
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
    locale_overlay: Option<&'a Map<String, Value>>,
    versioned_drafts: bool,
    held_gate: Option<&'a dyn HeldValueGate>,
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
            locale_overlay: None,
            versioned_drafts: false,
            held_gate: None,
        }
    }

    /// Set what of the stored sources the writer may lean on — see
    /// [`ValidationCtx::held_gate`].
    pub fn held_gate(mut self, held_gate: Option<&'a dyn HeldValueGate>) -> Self {
        self.held_gate = held_gate;
        self
    }

    /// Set whether the edited document keeps its drafts as version snapshots —
    /// see [`ValidationCtx::versioned_drafts`].
    pub fn versioned_drafts(mut self, versioned_drafts: bool) -> Self {
        self.versioned_drafts = versioned_drafts;
        self
    }

    /// Set the snapshot this write lands over the row after validation — see
    /// [`ValidationCtx::locale_overlay`].
    pub fn locale_overlay(mut self, locale_overlay: Option<&'a Map<String, Value>>) -> Self {
        self.locale_overlay = locale_overlay;
        self
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
            locale_overlay: self.locale_overlay,
            versioned_drafts: self.versioned_drafts,
            held_gate: self.held_gate,
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
        assert!(ctx.locale_overlay.is_none());
        assert!(!ctx.versioned_drafts);
    }

    /// The overlay is the snapshot a publish or a restore writes back after
    /// validation; the builder must carry it through to the context the
    /// completeness gate reads.
    #[test]
    fn builder_carries_the_locale_overlay() {
        let conn = InMemoryConn::open();
        let mut overlay = Map::new();
        overlay.insert("title__de".to_string(), Value::String("Hallo".into()));

        let ctx = ValidationCtx::builder(&conn, "posts")
            .locale_overlay(Some(&overlay))
            .build();

        assert_eq!(
            ctx.locale_overlay.and_then(|o| o.get("title__de")),
            Some(&Value::String("Hallo".into()))
        );
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
            .versioned_drafts(true)
            .build();

        assert_eq!(ctx.exclude_id, Some("doc-7"));
        assert!(ctx.versioned_drafts);
        assert!(ctx.is_draft);
        assert!(!ctx.soft_delete);
        assert!(ctx.registry.is_some());
    }
}
