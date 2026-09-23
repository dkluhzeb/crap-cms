//! Shared per-event delivery gate for the live-mutation streams.
//!
//! Both the admin SSE stream ([`crate::admin::handlers::events`]) and the gRPC
//! `Subscribe` stream ([`crate::api::handlers::subscribe`]) must apply the exact
//! same security-critical sequence to every event before delivering it:
//!
//! 1. look up the subscriber's per-view access for the event's target+slug,
//! 2. drop the event if it carries no view metadata (fail-closed),
//! 3. gate it by the content view it belongs to (published/draft/trash),
//! 4. drop it if a row constraint doesn't match the event's gating snapshot —
//!    the stored row, carried for this check only and never delivered — or the
//!    event carries no snapshot to judge (fail-closed),
//! 5. in `Full` mode, run the data-aware field-read strip, then the API-hidden
//!    strip, then `after_read` on the stripped data (the same order as the
//!    normal read pipeline) — yielding the visible data map; in `Metadata`
//!    mode, emit no data.
//!
//! The `Full`-mode payload the strip starts from is the event's document: the
//! stored row, read-shaped and stripped for no one (as `before_broadcast` left
//! it) — never the document as reported to the writer. So what a subscriber
//! receives is exactly what its own read would return, whatever the writer
//! could read.
//!
//! Keeping this in one place means a change to the strip pipeline (e.g. a new
//! strip step) can't silently land in one surface but not the other. Each
//! caller adds only its own concerns around the result: the SSE side wraps it in
//! a JSON envelope with the `self` flag; the gRPC side filters by requested
//! operations and converts to proto.

use std::collections::HashMap;

use serde_json::{Map, Value};
use tracing::warn;

use crate::{
    core::{
        CollectionDefinition, Document, DocumentFields, GlobalDefinition, HookRef, LiveMode,
        MutationEvent, Registry,
        event::{EventOperation, EventTarget},
    },
    db::{AccessResult, DbConnection, EventViewGate, FilterClause},
    hooks::{AccessCheckInput, EventAfterReadInput, HookRunner},
    service::helpers::strip_unreadable_fields,
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
    /// Per-global content-view access (published, plus draft when the global
    /// has drafts; globals have no trash view).
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
        // (metadata-only collections, all deletes); so is the gating snapshot
        // the row constraint below is judged against.
        let constraints = views.constraints_for(view)?;

        if !constraints.is_empty() && !self.row_constraints_match(event, constraints) {
            return None;
        }

        if self.mode_for(event) != LiveMode::Full {
            return Some(Map::new()); // metadata mode: no data
        }

        Some(self.strip_full_payload(event, slug))
    }

    /// Whether the event's row satisfies a non-empty row constraint.
    ///
    /// Judged against the event's gating snapshot — the row as stored (for a
    /// delete, as it was just before removal), hidden and read-denied fields
    /// included, as the SQL read path filters — never against the delivered
    /// `data`, which `live_mode` may empty and `before_broadcast` may reshape.
    /// An event without a snapshot (from a node that predates it, or dropped to
    /// fit the transport's size cap) cannot be judged, so a constrained view
    /// never receives it (fail-closed). Field types (from the schema) make
    /// Checkbox/Number constraints match SQL, not a blind string compare.
    fn row_constraints_match(&self, event: &MutationEvent, constraints: &[FilterClause]) -> bool {
        let Some(snapshot) = &event.gate else {
            return false;
        };

        let slug: &str = event.collection.as_ref();
        let fields = match event.target {
            EventTarget::Collection => self
                .registry
                .get_collection(slug)
                .map(|d| d.fields.as_slice()),
            EventTarget::Global => self.registry.get_global(slug).map(|d| d.fields.as_slice()),
        }
        .unwrap_or(&[]);

        snapshot.matches(constraints, fields)
    }

    /// `Full`-mode payload: the data-aware field-read strip, then the
    /// document-independent API-hidden strip, then `after_read` enrichment —
    /// the same order as the normal read pipeline (`post_process`) — applied
    /// to the event's document, which is the stored row stripped for no one
    /// (see [`crate::service::EventRow`]).
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
        // connection-less on a pool VM — a rule doing CRUD fails closed. The
        // API-hidden strip follows it, as in every other read.
        strip_unreadable_fields(&field_defs, &mut visible, |visible| {
            self.hook_runner.strip_read_access_for_event(
                &field_defs,
                visible,
                &event.data,
                slug,
                self.user_doc,
            );
        });

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
    /// Per-global content-view access (published, plus draft when the global
    /// has drafts; globals have no trash view).
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
    /// the caller's connection. Every axis a slug has — published always, draft
    /// with a status axis, trash with soft-delete (collections only) — resolves
    /// through the same per-view path. Collections honor row filters; globals
    /// are allow/deny only — a returned filter table drops the view
    /// (fail-closed). A slug with no visible view is omitted.
    #[must_use]
    pub fn resolve(input: &EventAccessInput) -> Self {
        let mut map = Self::empty();

        for slug in input.collection_slugs {
            let Some(def) = input.registry.get_collection(slug) else {
                continue;
            };

            let gate = resolve_gate(input, &ViewRules::for_collection(def), slug, false);
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

            // Globals are allow/deny only on every axis (reject_constrained).
            let gate = resolve_gate(input, &ViewRules::for_global(def), slug, true);
            if !gate.any_visible() {
                continue;
            }

            map.global_modes.insert(slug.clone(), def.live_mode);
            map.global_views.insert(slug.clone(), gate);
        }

        map
    }
}

/// One optional content-view axis of a slug: absent (the slug has no such
/// view — no status axis, no soft delete) or present under an access rule
/// (`None` = unset, decided by the `default_deny` policy).
#[derive(Clone, Copy)]
enum ViewAxis<'a> {
    Absent,
    Present(Option<&'a HookRef>),
}

impl<'a> ViewAxis<'a> {
    fn when(present: bool, rule: Option<&'a HookRef>) -> Self {
        if present {
            Self::Present(rule)
        } else {
            Self::Absent
        }
    }

    fn resolve(
        self,
        input: &EventAccessInput,
        slug: &str,
        reject_constrained: bool,
    ) -> Option<Vec<FilterClause>> {
        match self {
            Self::Absent => None,
            Self::Present(rule) => resolve_view(input, rule, slug, reject_constrained),
        }
    }
}

/// The content views a slug exposes and the access rule behind each.
struct ViewRules<'a> {
    published: Option<&'a HookRef>,
    draft: ViewAxis<'a>,
    trash: ViewAxis<'a>,
}

impl<'a> ViewRules<'a> {
    fn for_collection(def: &'a CollectionDefinition) -> Self {
        Self {
            published: def.access.read.as_ref(),
            draft: ViewAxis::when(def.has_drafts(), def.access.resolve_draft()),
            trash: ViewAxis::when(def.soft_delete, def.access.resolve_trash()),
        }
    }

    /// A global has no soft delete, so it carries the published and draft
    /// axes only — the draft axis under exactly the rule a collection uses.
    fn for_global(def: &'a GlobalDefinition) -> Self {
        Self {
            published: def.access.read.as_ref(),
            draft: ViewAxis::when(def.has_drafts(), def.access.resolve_draft()),
            trash: ViewAxis::Absent,
        }
    }
}

/// Resolve every axis of `rules` through [`resolve_view`], so a collection and
/// a global with the same access shape gate their views identically.
fn resolve_gate(
    input: &EventAccessInput,
    rules: &ViewRules<'_>,
    slug: &str,
    reject_constrained: bool,
) -> EventViewGate {
    EventViewGate {
        published: resolve_view(input, rules.published, slug, reject_constrained),
        draft: rules.draft.resolve(input, slug, reject_constrained),
        trash: rules.trash.resolve(input, slug, reject_constrained),
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
            warn!("Subscribe access for '{slug}' denied: {e}");

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
            warn!(
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
mod tests;
