//! The live event a delete publishes, read from the row it removes.

use tracing::warn;

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, EventGateSnapshot, Registry, SharedCache, SharedEventTransport,
        event::EventViewMeta,
    },
    db::{DbConnection, LocaleContext, query},
    hooks::HookRunner,
    service::{AppInfra, ServiceContext, ServiceError, purge_document},
};

type Result<T> = std::result::Result<T, ServiceError>;

/// A deleted document's live event: the content view the row was last in and
/// its gating snapshot. Every delete — soft or hard, single or bulk, a purge of
/// the trash included — derives it through [`read_delete_event`].
#[derive(Clone)]
pub(crate) struct DeleteEvent {
    view: EventViewMeta,
    gate: EventGateSnapshot,
}

impl DeleteEvent {
    /// An event gated by `view`, judged against `gate`.
    #[cfg(test)]
    pub(crate) fn new(view: EventViewMeta, gate: EventGateSnapshot) -> Self {
        Self { view, gate }
    }

    /// The view the event is gated by and the row subscribers' constraints
    /// are judged against.
    pub(crate) fn into_parts(self) -> (EventViewMeta, EventGateSnapshot) {
        (self.view, self.gate)
    }
}

/// Read the delete event of row `id` from the row as it stands on the
/// delete's own connection — for a hard delete just before it goes, for a
/// soft delete as it now sits in the trash. `None` when there is no such row.
///
/// The event is gated by the view the row was last in: the trash for a
/// trashed row (a soft delete, a purge of the trash, a forced delete of a
/// trashed document), otherwise its status view. A definition turned into its
/// hard-delete variant selects no trash column, so the stored `_deleted_at` is
/// read on its own and put back on the row — for the view, and for a trash
/// view's row constraint, which may name it.
///
/// # Errors
///
/// Returns a backend error if a read fails.
pub(crate) fn read_delete_event(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<DeleteEvent>> {
    let slug = &def.slug;

    let Some(mut row) = query::find_by_id_unfiltered(conn, slug, def, id, locale_ctx)? else {
        return Ok(None);
    };

    if !def.soft_delete
        && let Some(deleted_at) = query::stored_deleted_at(conn, slug, id)?
    {
        row.fields.insert("_deleted_at".to_string(), deleted_at);
    }

    Ok(Some(DeleteEvent {
        view: EventViewMeta::from_fields(&row.fields),
        gate: EventGateSnapshot::of(&row),
    }))
}

/// One purged document's delete event, held until its purge commits.
struct PurgedRow {
    collection: String,
    id: String,
    event: DeleteEvent,
}

/// A row whose delete event has been read and that is not yet purged.
pub(crate) struct CapturedPurge<'a> {
    id: &'a str,
    event: Option<DeleteEvent>,
}

/// The delete events of a batch of purges that run outside the service
/// delete — the retention purge and the CLI trash purge — captured before each
/// row goes and published once the purge's transaction has committed, as every
/// other write's events are.
///
/// A collector made with `capture = false` (no event transport) reads nothing.
/// Either way it records whether any row went, so [`settle`](Self::settle)
/// invalidates the cache exactly like the service delete does.
pub struct PurgeEvents {
    capture: bool,
    purged_any: bool,
    rows: Vec<PurgedRow>,
}

impl PurgeEvents {
    /// A collector that captures events when `capture` is set.
    #[must_use]
    pub fn new(capture: bool) -> Self {
        Self {
            capture,
            purged_any: false,
            rows: Vec::new(),
        }
    }

    /// Hard-delete `id` through [`purge_document`], first capturing its
    /// delete event. Returns whether a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the event read or the purge fails.
    pub(crate) fn purge(
        &mut self,
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        id: &str,
        locale_config: &LocaleConfig,
    ) -> Result<bool> {
        let captured = self.capture(conn, def, id, locale_config)?;

        self.complete(conn, def, captured, locale_config)
    }

    /// Read `id`'s delete event ahead of its purge — the read half of
    /// [`purge`](Self::purge), for a caller that must finish every read of a
    /// row before it writes anything. Reads nothing when not capturing.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the event read fails.
    pub(crate) fn capture<'a>(
        &self,
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        id: &'a str,
        locale_config: &LocaleConfig,
    ) -> Result<CapturedPurge<'a>> {
        if !self.capture {
            return Ok(CapturedPurge { id, event: None });
        }

        let locale_ctx = LocaleContext::default_for(locale_config);
        let event = read_delete_event(conn, def, id, locale_ctx.as_ref())?;

        Ok(CapturedPurge { id, event })
    }

    /// Hard-delete a [`capture`](Self::capture)d row through
    /// [`purge_document`], holding its event for [`publish`](Self::publish).
    /// Returns whether a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the purge fails.
    pub(crate) fn complete(
        &mut self,
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        captured: CapturedPurge<'_>,
        locale_config: &LocaleConfig,
    ) -> Result<bool> {
        let CapturedPurge { id, event } = captured;

        if !purge_document(conn, def, id, locale_config)? {
            return Ok(false);
        }

        self.purged_any = true;

        if let Some(event) = event {
            self.rows.push(PurgedRow {
                collection: def.slug.to_string(),
                id: id.to_string(),
                event,
            });
        }

        Ok(true)
    }

    /// Everything a committed purge owes the rest of the system — the same
    /// after-commit effects the service delete has: clear the populate cache
    /// when any row went, then publish every captured event. Call only once
    /// the purge's transaction has committed.
    pub fn settle(self, infra: &AppInfra) {
        self.clear_cache_if_purged(&infra.cache);

        self.publish(
            &infra.registry,
            &infra.hook_runner,
            infra.event_transport.as_ref(),
        );
    }

    /// Clear `cache` when this purge removed a row: a cached populate may
    /// still hold the purged document.
    fn clear_cache_if_purged(&self, cache: &SharedCache) {
        if !self.purged_any {
            return;
        }

        if let Err(e) = cache.clear() {
            warn!("Cache clear after purge failed: {e:#}");
        }
    }

    /// Publish every captured event — through the purged collections'
    /// definitions in `registry`, their `live` filters and `before_broadcast`
    /// hooks on `runner`, onto `transport`.
    fn publish(
        self,
        registry: &Registry,
        runner: &HookRunner,
        transport: Option<&SharedEventTransport>,
    ) {
        for PurgedRow {
            collection,
            id,
            event,
        } in self.rows
        {
            let Some(def) = registry.get_collection(&collection) else {
                continue;
            };

            let ctx = ServiceContext::collection(&collection, def)
                .runner(runner)
                .event_transport(transport.cloned())
                .build();

            ctx.publish_delete_event(&id, Some(event));
        }
    }
}

#[cfg(test)]
impl PurgeEvents {
    /// The captured events' document ids and views, in purge order.
    pub(crate) fn captured(&self) -> Vec<(&str, &EventViewMeta)> {
        self.rows
            .iter()
            .map(|row| (row.id.as_str(), &row.event.view))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::cache::MemoryCache;

    fn warm_cache() -> SharedCache {
        let cache: SharedCache = Arc::new(MemoryCache::new(16));
        cache.set("populate:posts:p1", b"{}").unwrap();

        cache
    }

    /// Regression: the retention and CLI trash purges never cleared the
    /// populate cache, though every service delete does.
    #[test]
    fn a_purge_that_removed_a_row_clears_the_cache() {
        let cache = warm_cache();
        let mut events = PurgeEvents::new(false);
        events.purged_any = true;

        events.clear_cache_if_purged(&cache);

        assert!(!cache.has("populate:posts:p1").unwrap());
    }

    #[test]
    fn a_purge_that_removed_nothing_keeps_the_cache() {
        let cache = warm_cache();

        PurgeEvents::new(true).clear_cache_if_purged(&cache);

        assert!(cache.has("populate:posts:p1").unwrap());
    }
}
