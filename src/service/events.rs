//! Shared per-event delivery gate for the live-mutation streams.
//!
//! Both the admin SSE stream ([`crate::admin::handlers::events`]) and the gRPC
//! `Subscribe` stream ([`crate::api::handlers::subscribe`]) must apply the exact
//! same security-critical sequence to every event before delivering it:
//!
//! 1. look up the subscriber's per-view access for the event's target+slug,
//! 2. drop the event if it carries no view metadata (fail-closed),
//! 3. gate it by the content view it belongs to (published/draft/trash),
//! 4. drop it if a row constraint doesn't match the payload,
//! 5. in `Full` mode, run the data-aware field-read strip, then the API-hidden
//!    strip, then `after_read` on the stripped data (the same order as the
//!    normal read pipeline) — yielding the visible data map; in `Metadata`
//!    mode, emit no data.
//!
//! Keeping this in one place means a change to the strip pipeline (e.g. a new
//! strip step) can't silently land in one surface but not the other. Each
//! caller adds only its own concerns around the result: the SSE side wraps it in
//! a JSON envelope with the `self` flag; the gRPC side filters by requested
//! operations and converts to proto.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::{
    core::{
        Document, DocumentFields, LiveMode, MutationEvent, Registry,
        event::{EventOperation, EventTarget},
    },
    db::{EventViewGate, query::filter::memory::matches_constraints_typed},
    hooks::{EventAfterReadInput, HookRunner},
    service::helpers::collect_api_hidden_field_names,
};

/// Map an event operation to the canonical lowercase string used by hooks,
/// constraint matching, and the requested-ops filter. Shared so the two stream
/// surfaces can't drift on the spelling.
#[must_use]
pub fn event_op_str(op: &EventOperation) -> &'static str {
    match op {
        EventOperation::Create => "create",
        EventOperation::Update => "update",
        EventOperation::Delete => "delete",
    }
}

/// Borrowed view of a subscriber's resolved access, plus the registry and hook
/// runner needed to process an event. Both stream surfaces build their own
/// owned access struct at connection time and hand a borrow here per event.
pub struct EventGate<'a> {
    /// Per-collection content-view access (published/draft/trash).
    pub collection_views: &'a HashMap<String, EventViewGate>,
    /// Per-global content-view access (globals carry only the published view).
    pub global_views: &'a HashMap<String, EventViewGate>,
    /// Delivery mode per collection slug. Split from globals because a
    /// collection and a global may share a slug (tables are namespaced).
    pub collection_modes: &'a HashMap<String, LiveMode>,
    /// Delivery mode per global slug.
    pub global_modes: &'a HashMap<String, LiveMode>,
    pub registry: &'a Registry,
    pub hook_runner: &'a HookRunner,
    /// The subscriber's user document, for per-user `after_read` + field access.
    pub user_doc: Option<&'a Document>,
}

impl EventGate<'_> {
    fn views_for(&self, event: &MutationEvent) -> Option<&EventViewGate> {
        let slug: &str = event.collection.as_ref();
        match event.target {
            EventTarget::Collection => self.collection_views.get(slug),
            EventTarget::Global => self.global_views.get(slug),
        }
    }

    fn mode_for(&self, event: &MutationEvent) -> LiveMode {
        let slug: &str = event.collection.as_ref();
        match event.target {
            EventTarget::Collection => self.collection_modes.get(slug),
            EventTarget::Global => self.global_modes.get(slug),
        }
        .copied()
        .unwrap_or_default()
    }

    /// Run the full per-event gate + strip pipeline. Returns the visible data
    /// map (empty in `Metadata` mode) when the subscriber may receive this
    /// event, or `None` when it must be dropped. Every drop point fails closed.
    #[must_use]
    pub fn evaluate(&self, event: &MutationEvent) -> Option<Map<String, Value>> {
        let slug: &str = event.collection.as_ref();
        let views = self.views_for(event)?;

        // Fail closed: an event without view metadata (e.g. from a pre-view node
        // during a rolling upgrade) cannot be safely gated, so drop it rather
        // than default to the published view.
        let view = event.view.as_ref()?;

        // Gate by the content view this event belongs to (published/draft/trash).
        // `None` means the subscriber cannot see that view, so the event is
        // dropped — closing the draft/trash leak. The `view` metadata is carried
        // independent of `live_mode`, so this holds for empty-`data` events too
        // (metadata-only collections, all deletes).
        let constraints = views.constraints_for(view)?;

        // Row-level constraints match against the event payload; empty `data`
        // cannot satisfy a non-empty constraint (fail-closed). Field types
        // (from the schema) make Checkbox/Number constraints match SQL, not a
        // blind string compare.
        let fields = match event.target {
            EventTarget::Collection => self
                .registry
                .get_collection(slug)
                .map(|d| d.fields.as_slice()),
            EventTarget::Global => self.registry.get_global(slug).map(|d| d.fields.as_slice()),
        }
        .unwrap_or(&[]);

        if !constraints.is_empty() && !matches_constraints_typed(&event.data, constraints, fields) {
            return None;
        }

        if self.mode_for(event) != LiveMode::Full {
            return Some(Map::new()); // metadata mode: no data
        }

        Some(self.strip_full_payload(event, slug))
    }

    /// `Full`-mode payload: the data-aware field-read strip, then the
    /// document-independent API-hidden strip, then `after_read` enrichment —
    /// the same order as the normal read pipeline (`post_process`).
    ///
    /// Strip-before-`after_read` is load-bearing: the per-subscriber
    /// `after_read` hook must only ever see the already-access-stripped form
    /// (as documented), otherwise it could copy a read-denied field's value
    /// into an unprotected field that survives the strip — leaking it to a
    /// subscriber the access rule denies.
    fn strip_full_payload(&self, event: &MutationEvent, slug: &str) -> Map<String, Value> {
        let (hooks, field_defs) = match event.target {
            EventTarget::Collection => self
                .registry
                .get_collection(slug)
                .map(|d| (d.hooks.clone(), d.fields.clone())),
            EventTarget::Global => self
                .registry
                .get_global(slug)
                .map(|d| (d.hooks.clone(), d.fields.clone())),
        }
        .unwrap_or_default();

        let mut visible: Map<String, Value> = event
            .data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Data-aware field-read strip (each `access.read` rule sees the event's
        // original document as `ctx.data` / `ctx.document`, matching the
        // per-level snapshot semantics of normal reads), evaluated
        // connection-less on a pool VM — a rule doing CRUD fails closed.
        self.hook_runner.strip_read_access_for_event(
            &field_defs,
            &mut visible,
            &event.data,
            slug,
            self.user_doc,
        );

        for denial in collect_api_hidden_field_names(&field_defs, "") {
            denial.strip_from(&mut visible);
        }

        // Per-subscriber `after_read` enrichment on the stripped data.
        let stripped: DocumentFields = visible
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let processed = self
            .hook_runner
            .apply_after_read_for_event(&EventAfterReadInput {
                collection: slug,
                hooks: &hooks,
                fields: &field_defs,
                document_id: event.document_id.as_ref(),
                data: &stripped,
                user: self.user_doc,
                operation: event_op_str(&event.operation),
                timestamp: event.timestamp.as_str(),
            });

        processed
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;

    use crate::config::CrapConfig;
    use crate::core::event::EventViewMeta;
    use crate::core::{DocumentFields, LiveMode, MutationEvent};
    use crate::db::EventViewGate;
    use crate::hooks::{self, lifecycle::HookRunner};

    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
    }

    /// Regression: the Full-mode event pipeline ran per-subscriber
    /// `after_read` hooks BEFORE the field-read strip (normal reads strip
    /// first). A hook copying a read-denied field's value into an
    /// unprotected field leaked it past the strip to a denied subscriber.
    #[test]
    fn full_payload_strips_before_after_read() {
        let config_dir = fixture_dir();
        let config = CrapConfig::test_default();
        let registry = hooks::init_lua(&config_dir, &config).unwrap();
        let runner = HookRunner::builder()
            .config_dir(&config_dir)
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("secret".to_string(), json!("s3cr3t-value"));

        let event = MutationEvent {
            sequence: 1,
            timestamp: "2026-08-11T00:00:00Z".to_string(),
            target: EventTarget::Collection,
            operation: EventOperation::Update,
            collection: "event_leak".into(),
            document_id: "d1".into(),
            data,
            edited_by: None,
            view: Some(EventViewMeta::default()),
        };

        let mut views = HashMap::new();
        views.insert(
            "event_leak".to_string(),
            EventViewGate {
                published: Some(vec![]),
                draft: None,
                trash: None,
            },
        );
        let mut modes = HashMap::new();
        modes.insert("event_leak".to_string(), LiveMode::Full);
        let empty_views = HashMap::new();
        let empty_modes = HashMap::new();

        let gate = EventGate {
            collection_views: &views,
            global_views: &empty_views,
            collection_modes: &modes,
            global_modes: &empty_modes,
            registry: &registry,
            hook_runner: &runner,
            user_doc: None,
        };

        let visible = gate.evaluate(&event).expect("event must be delivered");

        assert!(
            visible.get("secret").is_none(),
            "read-denied field must be stripped from the payload"
        );

        let summary = visible
            .get("summary")
            .and_then(|v| v.as_str())
            .expect("after_read hook must have set summary");
        assert!(
            !summary.contains("s3cr3t-value"),
            "after_read must not see the denied field's value; got: {summary}"
        );
        assert_eq!(
            summary, "seen:nil",
            "the hook ran on the already-stripped data"
        );
    }
}
