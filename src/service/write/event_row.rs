//! The row a write's live event is built from, read in the default locale.
//!
//! A subscriber picks no locale: its `Full`-mode payload is what its own read
//! without a locale returns — the default locale's values — and a delete's
//! event is judged on the row read in the default locale too. A write made in
//! another locale reads its reported row in that locale, so its event re-reads
//! the row it stored in the default locale, inside the same transaction.

use anyhow::anyhow;

use crate::{
    config::LocaleConfig,
    core::{Document, EventViewPlacement},
    db::{LocaleContext, query},
    service::{
        Def, EventRow, RowBefore, ServiceContext, ServiceError,
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

    /// The row as stored before this write changes it (see [`RowBefore`]),
    /// read in the default locale the event is judged in, under the write's
    /// row lock, before it persists — for a write whose event announces a
    /// move that changes the row's content too (a version restore): the view
    /// the row left is judged against what it held there. `None` when this
    /// operation publishes no event; `locale_config` falls back to the
    /// context's.
    pub(crate) fn row_before_write(
        &self,
        id: &str,
        locale_config: Option<&LocaleConfig>,
    ) -> Result<Option<RowBefore>> {
        if !self.publishes_events() {
            return Ok(None);
        }

        let default = locale_config
            .or(self.locale_config)
            .and_then(LocaleContext::default_for);
        let row = self.stored_row(id, default.as_ref())?;

        Ok(Some(RowBefore::of(&row)))
    }

    /// The row as an update finds it, when its event needs it (see
    /// [`EventRow::before_write`]): a published update of a collection with
    /// drafts can move the row between the status views — a publish of a
    /// draft leaves the draft view, content and all — and a draft save
    /// (`draft`) describes a pending draft while the stored row stays where
    /// it is. `None` otherwise: without drafts an update never moves a row.
    ///
    /// Only the row's placement is read unless the update publishes a draft:
    /// a draft save leaves the stored row where it is, and a published update
    /// of a row outside the draft view keeps it in its view, so no view is
    /// judged against the content the row held. A publish of a draft is — and
    /// a row constraint can name any stored field, so then the whole row is
    /// read.
    pub(crate) fn update_row_before(
        &self,
        id: &str,
        draft: bool,
        write_locale: Option<&LocaleContext>,
    ) -> Result<Option<RowBefore>> {
        let has_drafts = match &self.def {
            Def::Collection(def) => def.has_drafts(),
            Def::Global(def) => draft && def.has_drafts(),
            Def::None => false,
        };

        if !has_drafts || !self.publishes_events() {
            return Ok(None);
        }

        let placement = self.stored_placement(id)?;

        if draft || !placement.is_draft() {
            return Ok(Some(RowBefore::placed(placement)));
        }

        self.row_before_write(id, write_locale.map(|lc| &lc.config))
    }

    /// Where the target's stored row sits across the content views, reading
    /// only its `_status` (and, for a collection with a trash, `_deleted_at`).
    fn stored_placement(&self, id: &str) -> Result<EventViewPlacement> {
        let conn = self.resolve_conn()?;
        let trash = matches!(&self.def, Def::Collection(def) if def.soft_delete);

        query::find_view_placement(conn.as_ref(), &self.version_table(), id, trash)?.ok_or_else(
            || ServiceError::Internal(anyhow!("Document {id} vanished from {}", self.slug)),
        )
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
            CollectionDefinition, EventViewMeta, FieldDefinition, FieldType, SharedEventTransport,
            VersionsConfig, event::InProcessEventBus,
        },
        db::{DbConnection, InMemoryConn, LocaleMode, query::test_helpers::CountingConn},
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

    /// The status view the row an update finds sits in, as its event
    /// records it: a published write moves from it, a draft save leaves it.
    fn recorded(before: Option<RowBefore>, draft: bool) -> EventViewMeta {
        let mut now = Document::new("p1".to_string());
        now.fields.insert("_status".into(), json!("published"));

        EventRow::new(&now).before_write(before, draft).view()
    }

    /// An update of a collection with drafts reads the row it finds — a
    /// published write to record the move it makes, a draft save to record
    /// where the stored row stays; a collection without drafts, or a write
    /// that publishes no event, reads nothing.
    #[test]
    fn update_row_before_reads_only_what_an_event_can_announce() {
        let conn = seeded();
        conn.execute("UPDATE posts SET _status = 'draft' WHERE id = 'p1'", &[])
            .unwrap();
        let config = locales();

        let drafted = drafted_posts();
        let publishing = ServiceContext::collection("posts", &drafted)
            .conn(&conn)
            .locale_config(Some(&config))
            .event_transport(Some(transport()))
            .build();

        let published = publishing.update_row_before("p1", false, None).unwrap();
        let view = recorded(published, false);
        assert_eq!(view.prior.and_then(|p| p.status).as_deref(), Some("draft"));

        let draft_save = publishing.update_row_before("p1", true, None).unwrap();
        assert!(draft_save.is_some(), "a draft save records the stored row");

        let plain = posts();
        let without_drafts = ServiceContext::collection("posts", &plain)
            .conn(&conn)
            .locale_config(Some(&config))
            .event_transport(Some(transport()))
            .build();
        assert!(
            without_drafts
                .update_row_before("p1", false, None)
                .unwrap()
                .is_none()
        );

        let silent = ServiceContext::collection("posts", &drafted)
            .conn(&conn)
            .locale_config(Some(&config))
            .build();
        assert!(
            silent
                .update_row_before("p1", false, None)
                .unwrap()
                .is_none()
        );
    }

    /// An update context over `conn` for the drafted `posts`, with events on.
    fn drafted_ctx<'a>(
        conn: &'a CountingConn<'a>,
        def: &'a CollectionDefinition,
        config: &'a LocaleConfig,
    ) -> ServiceContext<'a> {
        ServiceContext::collection("posts", def)
            .conn(conn)
            .locale_config(Some(config))
            .event_transport(Some(transport()))
            .build()
    }

    /// Regression: every update of a collection with drafts read the whole
    /// stored row — every column, every array, block and relationship row —
    /// though only a publish of a draft judges a view against the content the
    /// row held. A draft save and a published update of a published row now
    /// read the row's placement alone.
    #[test]
    fn only_a_publish_of_a_draft_reads_the_whole_row() {
        let seeded = seeded();
        let def = drafted_posts();
        let config = locales();

        seeded
            .execute(
                "UPDATE posts SET _status = 'published' WHERE id = 'p1'",
                &[],
            )
            .unwrap();

        let conn = CountingConn::new(&seeded);
        let ctx = drafted_ctx(&conn, &def, &config);
        ctx.update_row_before("p1", false, None).unwrap().unwrap();
        ctx.update_row_before("p1", true, None).unwrap().unwrap();
        assert_eq!(conn.reads(), 2, "one placement read per update");

        seeded
            .execute("UPDATE posts SET _status = 'draft' WHERE id = 'p1'", &[])
            .unwrap();

        let conn = CountingConn::new(&seeded);
        let ctx = drafted_ctx(&conn, &def, &config);
        let before = ctx.update_row_before("p1", false, None).unwrap();
        assert!(conn.reads() > 1, "a publish of a draft reads the row");

        let view = recorded(before, false);
        assert!(
            view.prior_gate.is_some(),
            "the draft view is judged against what the row held there"
        );
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
