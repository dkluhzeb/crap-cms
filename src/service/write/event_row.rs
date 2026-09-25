//! The row a write's live event is built from, read in the default locale.
//!
//! A subscriber picks no locale: its `Full`-mode payload is what its own read
//! without a locale returns — the default locale's values — and a delete's
//! event is judged on the row read in the default locale too. A write made in
//! another locale reads its reported row in that locale, so its event re-reads
//! the row it stored in the default locale, inside the same transaction.

use anyhow::anyhow;

use crate::{
    core::Document,
    db::{LocaleContext, query},
    service::{
        Def, EventRow, ServiceContext, ServiceError,
        persist::{DraftDocumentArgs, draft_document},
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

impl ServiceContext<'_> {
    /// The row this write's live event is built from: `doc`, the row as the
    /// write stored and read it in `write_locale` — or, for a write made in a
    /// locale other than the default, the same row re-read in the default
    /// locale. `draft` says the write saved a draft version (its reported row
    /// is the draft snapshot, not the stored row). `None` when this operation
    /// publishes no event — nothing is read then.
    pub(crate) fn write_event_row(
        &self,
        doc: &Document,
        write_locale: Option<&LocaleContext>,
        draft: bool,
    ) -> Result<Option<EventRow>> {
        if !self.publishes_events() {
            return Ok(None);
        }

        let Some(write_locale) = write_locale.filter(|lc| !reads_default_locale(lc)) else {
            return Ok(Some(EventRow::new(doc)));
        };

        let default = LocaleContext::default_for(&write_locale.config);

        let row = if draft {
            self.stored_draft(&doc.id, default.as_ref())?
        } else {
            self.stored_row(&doc.id, default.as_ref())?
        };

        Ok(Some(EventRow::new(&row)))
    }

    /// The stored `_status` of collection row `id` before this write changes
    /// it, for the write's live event to announce a move between the status
    /// views (see [`EventRow::status_moved_from`]) — a publish moves a draft
    /// out of the draft view. `None` when there is nothing to announce: this
    /// operation publishes no event, the collection has no drafts, or the
    /// write saves a draft version (`draft`), which leaves the stored row
    /// where it is. Read under the write's row lock, before it persists.
    pub(crate) fn status_before_write(&self, id: &str, draft: bool) -> Result<Option<String>> {
        let Def::Collection(def) = &self.def else {
            return Ok(None);
        };

        if draft || !def.has_drafts() || !self.publishes_events() {
            return Ok(None);
        }

        let conn = self.resolve_conn()?;

        Ok(query::get_document_status(conn.as_ref(), self.slug, id)?)
    }

    /// The target's stored row in `locale_ctx` as a published write reports
    /// it: a collection document with its join fields hydrated, or the global.
    fn stored_row(&self, id: &str, locale_ctx: Option<&LocaleContext>) -> Result<Document> {
        let conn = self.resolve_conn()?;
        let conn = conn.as_ref();

        if let Def::Global(def) = &self.def {
            return Ok(query::get_global(conn, self.slug, def, locale_ctx)?);
        }

        query::find_by_id(conn, self.slug, self.collection_def()?, id, locale_ctx)?.ok_or_else(
            || ServiceError::Internal(anyhow!("Document {id} vanished from {}", self.slug)),
        )
    }

    /// The draft a draft save reports, in `locale_ctx`: the version snapshot it
    /// just saved, read for that locale over the stored row.
    fn stored_draft(&self, id: &str, locale_ctx: Option<&LocaleContext>) -> Result<Document> {
        let existing = self.stored_row(id, locale_ctx)?;

        let conn = self.resolve_conn()?;
        let Some(version) = query::find_latest_version(conn.as_ref(), &self.version_table(), id)?
        else {
            return Ok(existing);
        };

        Ok(draft_document(&DraftDocumentArgs {
            id,
            snapshot: &version.snapshot,
            existing: &existing,
            fields: self.fields()?,
            locale_ctx,
        })?)
    }
}

/// Whether a write in `locale_ctx` reads its row as the default locale does —
/// the default locale itself (or no single locale), whose values need no
/// fallback.
fn reads_default_locale(locale_ctx: &LocaleContext) -> bool {
    locale_ctx.access_locale() == locale_ctx.config.default_locale
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{
            CollectionDefinition, FieldDefinition, FieldType, SharedEventTransport, VersionsConfig,
            event::InProcessEventBus,
        },
        db::{DbConnection, InMemoryConn, LocaleMode},
    };

    fn locales() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn posts() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
        ];
        def
    }

    fn transport() -> SharedEventTransport {
        Arc::new(InProcessEventBus::new(16))
    }

    /// A `posts` row whose `en` and `de` titles differ.
    fn seeded() -> InMemoryConn {
        let conn = InMemoryConn::open();

        conn.execute_ddl(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title__en TEXT, title__de TEXT, \
             _status TEXT, created_at TEXT, updated_at TEXT, _ref_count INTEGER DEFAULT 0)",
            &[],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, title__en, title__de) VALUES ('p1', 'Hello', 'Hallo')",
            &[],
        )
        .unwrap();

        conn
    }

    fn event_title(row: Option<EventRow>) -> Value {
        let doc = row.expect("row captured").into_document();

        doc.fields.get("title").cloned().unwrap_or_default()
    }

    /// Regression: a `Full`-mode event of a write made in `de` carried the
    /// `de` values, while a subscriber — who picks no locale — reads the
    /// default locale's, as a delete's event does. The event row is re-read in
    /// the default locale.
    #[test]
    fn a_non_default_locale_write_publishes_the_default_locale_row() {
        let conn = seeded();
        let def = posts();
        let config = locales();
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .locale_config(Some(&config))
            .event_transport(Some(transport()))
            .build();
        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: config.clone(),
        };

        let mut written = Document::new("p1".to_string());
        written.fields.insert("title".to_string(), json!("Hallo"));

        let row = ctx.write_event_row(&written, Some(&de), false).unwrap();

        assert_eq!(event_title(row), json!("Hello"));
    }

    /// A write in the default locale publishes the row it read — no re-read.
    #[test]
    fn a_default_locale_write_publishes_its_own_row() {
        let def = posts();
        let config = locales();
        let conn = InMemoryConn::open();
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .event_transport(Some(transport()))
            .build();
        let en = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config,
        };

        let mut written = Document::new("p1".to_string());
        written.fields.insert("title".to_string(), json!("Hello"));

        let row = ctx.write_event_row(&written, Some(&en), false).unwrap();

        assert_eq!(event_title(row), json!("Hello"));
    }

    fn drafted_posts() -> CollectionDefinition {
        let mut def = posts();
        def.versions = Some(VersionsConfig::new(true, 0));
        def
    }

    /// A write that may publish a draft reads the status the row has going
    /// in; a draft save, a collection without drafts, or a write that
    /// publishes no event reads nothing.
    #[test]
    fn status_before_write_reads_only_what_an_event_can_announce() {
        let conn = seeded();
        conn.execute("UPDATE posts SET _status = 'draft' WHERE id = 'p1'", &[])
            .unwrap();

        let drafted = drafted_posts();
        let publishing = ServiceContext::collection("posts", &drafted)
            .conn(&conn)
            .event_transport(Some(transport()))
            .build();

        assert_eq!(
            publishing
                .status_before_write("p1", false)
                .unwrap()
                .as_deref(),
            Some("draft")
        );
        assert_eq!(publishing.status_before_write("p1", true).unwrap(), None);

        let plain = posts();
        let without_drafts = ServiceContext::collection("posts", &plain)
            .conn(&conn)
            .event_transport(Some(transport()))
            .build();
        assert_eq!(
            without_drafts.status_before_write("p1", false).unwrap(),
            None
        );

        let silent = ServiceContext::collection("posts", &drafted)
            .conn(&conn)
            .build();
        assert_eq!(silent.status_before_write("p1", false).unwrap(), None);
    }

    /// Nothing is read for a write that publishes no event.
    #[test]
    fn no_event_reads_nothing() {
        let def = posts();
        let config = locales();
        let conn = InMemoryConn::open();
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .build();
        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config,
        };

        let written = Document::new("p1".to_string());

        assert!(
            ctx.write_event_row(&written, Some(&de), false)
                .unwrap()
                .is_none()
        );
    }
}
