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
        Document, DocumentFields, HookRef, LiveMode, MutationEvent, Registry,
        event::{EventOperation, EventTarget},
    },
    db::{
        AccessResult, DbConnection, EventViewGate, FilterClause,
        query::filter::memory::matches_constraints_typed,
    },
    hooks::{AccessCheckInput, EventAfterReadInput, HookRunner},
    service::helpers::collect_api_hidden_field_names,
};

/// Map an event operation to the canonical lowercase string used by hooks,
/// constraint matching, and the requested-ops filter. Shared so the two stream
/// surfaces can't drift on the spelling.
#[must_use]
pub fn event_op_str(op: &EventOperation) -> &'static str {
    op.as_str()
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

/// Inputs for [`EventAccessMap::resolve`]: the requested slugs plus everything a
/// per-view access hook needs. Both stream surfaces populate this identically.
pub struct EventAccessInput<'a> {
    pub registry: &'a Registry,
    pub collection_slugs: &'a [String],
    pub global_slugs: &'a [String],
    pub user_doc: Option<&'a Document>,
    pub hook_runner: &'a HookRunner,
    pub conn: &'a dyn DbConnection,
}

/// A subscriber's owned per-view access maps, built once at connection time.
///
/// The construction companion to [`EventGate`] (which shares the per-event
/// enforcement): both the admin SSE stream and the gRPC `Subscribe` stream build
/// this via [`resolve`](Self::resolve) so the security-critical access
/// resolution — the fail-closed hook mapping, the per-axis view gating, the
/// globals-are-allow/deny-only rule — can't drift between the two surfaces.
#[derive(Default)]
pub struct EventAccessMap {
    /// Per-collection content-view access (published/draft/trash).
    pub collection_views: HashMap<String, EventViewGate>,
    /// Per-global content-view access (globals carry only the published view).
    pub global_views: HashMap<String, EventViewGate>,
    /// Delivery mode per collection slug (split from globals — a collection and a
    /// global may share a slug, tables being namespaced).
    pub collection_modes: HashMap<String, LiveMode>,
    /// Delivery mode per global slug.
    pub global_modes: HashMap<String, LiveMode>,
}

impl EventAccessMap {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Resolve per-view access for the requested collections and globals under
    /// the caller's connection. Collections carry all three view axes (draft only
    /// with a status axis, trash only with soft-delete) and honor row filters;
    /// globals carry only the published view and are allow/deny only — a returned
    /// filter table drops the view (fail-closed). A slug with no visible view is
    /// omitted.
    #[must_use]
    pub fn resolve(input: &EventAccessInput) -> Self {
        let mut map = Self::empty();

        for slug in input.collection_slugs {
            let Some(def) = input.registry.get_collection(slug) else {
                continue;
            };

            let gate = EventViewGate {
                published: resolve_view(input, def.access.read.as_ref(), slug, false),
                draft: def
                    .has_drafts()
                    .then(|| resolve_view(input, def.access.resolve_draft(), slug, false))
                    .flatten(),
                trash: def
                    .soft_delete
                    .then(|| resolve_view(input, def.access.resolve_trash(), slug, false))
                    .flatten(),
            };

            if !gate.any_visible() {
                continue;
            }

            map.collection_modes.insert(slug.clone(), def.live_mode);
            map.collection_views.insert(slug.clone(), gate);
        }

        for slug in input.global_slugs {
            let Some(def) = input.registry.get_global(slug) else {
                continue;
            };

            // Globals: single published row, allow/deny only (reject_constrained).
            let gate = EventViewGate {
                published: resolve_view(input, def.access.read.as_ref(), slug, true),
                draft: None,
                trash: None,
            };

            if !gate.any_visible() {
                continue;
            }

            map.global_modes.insert(slug.clone(), def.live_mode);
            map.global_views.insert(slug.clone(), gate);
        }

        map
    }
}

/// Run one view's access hook and map the outcome to visibility: `Some(filters)`
/// when allowed (empty for unconstrained), `None` when denied or the hook errors
/// (fail-closed).
fn resolve_view(
    input: &EventAccessInput,
    access_ref: Option<&HookRef>,
    slug: &str,
    reject_constrained: bool,
) -> Option<Vec<FilterClause>> {
    match input.hook_runner.check_access(
        &AccessCheckInput::builder("subscribe", slug)
            .access(access_ref)
            .user(input.user_doc)
            .build(),
        input.conn,
    ) {
        Ok(result) => view_from_access(result, reject_constrained, slug),
        // Fail-closed: an access hook that errors — including a row constraint
        // rejected by the operator allowlist — hides the view rather than
        // streaming events past an unvalidated constraint.
        Err(e) => {
            tracing::warn!("Subscribe access for '{slug}' denied: {e}");
            None
        }
    }
}

/// Map an access-check outcome to view visibility. Globals (`reject_constrained`)
/// drop a filter-table result (fail-closed) because they are allow/deny only and
/// every synchronous global path rejects a constraint as a config error; the live
/// stream can't hard-error, so it hides the view instead of applying a row filter
/// globals don't honor. Collections honor the filter as a row constraint.
#[must_use]
fn view_from_access(
    result: AccessResult,
    reject_constrained: bool,
    slug: &str,
) -> Option<Vec<FilterClause>> {
    match result {
        AccessResult::Allowed => Some(Vec::new()),
        AccessResult::Constrained(_) if reject_constrained => {
            tracing::warn!(
                "Subscribe access for global '{slug}' returned a filter table; \
                 globals are allow/deny only — hiding the view"
            );
            None
        }
        AccessResult::Constrained(filters) => Some(filters),
        AccessResult::Denied => None,
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

    /// Regression: a global access hook that returns a filter table is a config
    /// error every synchronous global path rejects. On the live streams it must
    /// fail closed (drop the view), never apply a row filter globals don't honor.
    /// Collections, by contrast, keep the constraint as a row filter.
    #[test]
    fn global_constrained_view_is_dropped_collection_is_kept() {
        use crate::db::{AccessResult, FilterClause};

        let filters = vec![FilterClause::and(Vec::new())];

        // Global (reject_constrained = true): filter table → hidden.
        assert!(
            view_from_access(AccessResult::Constrained(filters.clone()), true, "settings")
                .is_none(),
            "a global returning a filter table must drop the view (fail-closed)"
        );

        // Collection (reject_constrained = false): filter table → honored.
        let kept = view_from_access(AccessResult::Constrained(filters), false, "posts");
        assert_eq!(
            kept.as_ref().map(Vec::len),
            Some(1),
            "a collection returning a filter table keeps it as a row constraint"
        );

        // Allow/deny map the same way regardless of the flag.
        assert_eq!(
            view_from_access(AccessResult::Allowed, true, "settings").map(|f| f.len()),
            Some(0),
            "Allowed yields an unconstrained (empty-filter) view"
        );
        assert!(view_from_access(AccessResult::Denied, false, "posts").is_none());
    }
}
