//! Access resolution and JSON payload construction for the admin SSE stream.
//!
//! Separated from the streaming/handler plumbing in [`super::sse`] so the
//! security-critical gating (per-view access, field stripping, editor-identity
//! suppression) lives in one focused, unit-testable place.

use std::collections::HashMap;

use axum::response::sse::Event;
use serde_json::{Map, Value, json};
use tracing::warn;

use crate::{
    admin::AdminState,
    core::{
        Document, FieldDenial, HookRef, LiveMode, MutationEvent, Registry,
        event::{EventOperation, EventTarget},
    },
    db::{AccessResult, DbConnection, EventViewGate, FilterClause, query::filter::memory},
    hooks::{AccessCheckInput, EventAfterReadInput, HookRunner},
};

/// Resolved SSE access: per-view visibility (with row constraints) for
/// collections and globals, field denials, and modes.
pub(super) struct SseAccess {
    collection_views: HashMap<String, EventViewGate>,
    global_views: HashMap<String, EventViewGate>,
    // Field denials and modes are split by target: a collection and a global
    // may share a slug (tables are namespaced), so a single slug-keyed map would
    // let one clobber the other and leak a denied field or full payload.
    collection_denied_fields: HashMap<String, Vec<FieldDenial>>,
    global_denied_fields: HashMap<String, Vec<FieldDenial>>,
    collection_modes: HashMap<String, LiveMode>,
    global_modes: HashMap<String, LiveMode>,
}

impl SseAccess {
    pub(super) fn empty() -> Self {
        Self {
            collection_views: HashMap::new(),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: HashMap::new(),
            global_modes: HashMap::new(),
        }
    }
}

/// Run one content view's access hook and map the outcome to visibility:
/// `Some(filters)` when allowed (empty for unconstrained), `None` when denied or
/// the hook errors (fail-closed). With `reject_constrained` (globals), a returned
/// filter table drops the view rather than applying a row filter globals don't
/// honor — matching the synchronous global paths that reject it as a config error.
fn resolve_view(
    hook_runner: &HookRunner,
    access_ref: Option<&HookRef>,
    user_doc: Option<&Document>,
    conn: &dyn DbConnection,
    slug: &str,
    reject_constrained: bool,
) -> Option<Vec<FilterClause>> {
    match hook_runner.check_access(
        &AccessCheckInput {
            access: access_ref,
            user: user_doc,
            id: None,
            data: None,
            locale: None,
            operation: "subscribe",
            collection: slug,
            ui_locale: None,
        },
        conn,
    ) {
        Ok(result) => view_from_access(result, reject_constrained, slug),
        // Fail-closed: an access hook that errors — including a row constraint
        // rejected by the operator allowlist — hides the view rather than
        // streaming events past an unvalidated constraint.
        Err(e) => {
            warn!("SSE access for '{slug}' denied: {e}");
            None
        }
    }
}

/// Map an access-check outcome to view visibility. Globals (`reject_constrained`)
/// drop a filter-table result (fail-closed) because they are allow/deny only and
/// every synchronous global path rejects a constraint as a config error; the live
/// stream can't hard-error, so it hides the view instead of applying a row filter
/// globals don't honor. Collections honor the filter as a row constraint.
fn view_from_access(
    result: AccessResult,
    reject_constrained: bool,
    slug: &str,
) -> Option<Vec<FilterClause>> {
    match result {
        AccessResult::Allowed => Some(Vec::new()),
        AccessResult::Constrained(_) if reject_constrained => {
            warn!(
                "SSE access for global '{slug}' returned a filter table; \
                 globals are allow/deny only — hiding the view"
            );
            None
        }
        AccessResult::Constrained(filters) => Some(filters),
        AccessResult::Denied => None,
    }
}

/// Build per-view access for every collection/global the user can see, caching
/// field-level read denials and live mode per slug for stream filtering.
pub(super) fn build_allowed_slugs(state: &AdminState, user_doc: Option<&Document>) -> SseAccess {
    let mut access = SseAccess::empty();

    let Ok(mut conn) = state.pool.get() else {
        return access;
    };

    let Ok(tx) = conn.transaction() else {
        return access;
    };

    let runner = &state.hook_runner;

    for (slug, def) in &state.registry.collections {
        // Draft view only exists with a status axis; trash only with soft-delete.
        let gate = EventViewGate {
            published: resolve_view(runner, def.access.read.as_ref(), user_doc, &tx, slug, false),
            draft: def
                .has_drafts()
                .then(|| {
                    resolve_view(
                        runner,
                        def.access.resolve_draft(),
                        user_doc,
                        &tx,
                        slug,
                        false,
                    )
                })
                .flatten(),
            trash: def
                .soft_delete
                .then(|| {
                    resolve_view(
                        runner,
                        def.access.resolve_trash(),
                        user_doc,
                        &tx,
                        slug,
                        false,
                    )
                })
                .flatten(),
        };

        if !gate.any_visible() {
            continue;
        }

        let denied = runner.check_field_read_access(&def.fields, user_doc, None, &tx);

        if !denied.is_empty() {
            access
                .collection_denied_fields
                .insert(slug.to_string(), denied);
        }

        access
            .collection_modes
            .insert(slug.to_string(), def.live_mode);
        access.collection_views.insert(slug.to_string(), gate);
    }

    for (slug, def) in &state.registry.globals {
        // Globals are a single published row — no status or trash axis. They are
        // allow/deny only, so a Constrained result drops the view (fail-closed).
        let gate = EventViewGate {
            published: resolve_view(runner, def.access.read.as_ref(), user_doc, &tx, slug, true),
            draft: None,
            trash: None,
        };

        if !gate.any_visible() {
            continue;
        }

        let denied = runner.check_field_read_access(&def.fields, user_doc, None, &tx);

        if !denied.is_empty() {
            access.global_denied_fields.insert(slug.to_string(), denied);
        }

        access.global_modes.insert(slug.to_string(), def.live_mode);
        access.global_views.insert(slug.to_string(), gate);
    }

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    access
}

/// Build the JSON payload for an SSE event, applying access control, `after_read` hooks,
/// and field stripping. Returns `None` when the subscriber should not receive this event.
///
/// Separated from [`event_to_sse`] so it can be unit-tested without depending on
/// axum's opaque `sse::Event` body representation.
fn build_event_payload(
    event: &MutationEvent,
    access: &SseAccess,
    hook_runner: &HookRunner,
    registry: &Registry,
    user_doc: Option<&Document>,
) -> Option<Value> {
    let slug_str: &str = event.collection.as_ref();

    let views = match event.target {
        EventTarget::Collection => access.collection_views.get(slug_str),
        EventTarget::Global => access.global_views.get(slug_str),
    }?;

    // Fail closed: an event without view metadata (e.g. from a pre-view node
    // during a rolling upgrade) cannot be safely gated, so drop it rather than
    // default to the published view.
    let view = event.view.as_ref()?;

    // Gate by the content view this event belongs to (trash/draft/published).
    // `None` means the subscriber cannot see that view, so the event is dropped
    // — closing the draft/trash leak. The `view` metadata is carried
    // independent of `live_mode`, so this holds for empty-`data` events too
    // (metadata-only collections, all deletes).
    let constraints = views.constraints_for(view)?;

    // Row-level constraints match against the event payload; empty `data`
    // cannot satisfy a non-empty constraint (fail-closed) — unchanged behavior.
    // Field types make Checkbox/Number constraints match SQL, not a string compare.
    let fields = match event.target {
        EventTarget::Collection => registry
            .get_collection(slug_str)
            .map(|d| d.fields.as_slice()),
        EventTarget::Global => registry.get_global(slug_str).map(|d| d.fields.as_slice()),
    }
    .unwrap_or(&[]);

    if !constraints.is_empty()
        && !memory::matches_constraints_typed(&event.data, constraints, fields)
    {
        return None;
    }

    let target_str = match event.target {
        EventTarget::Collection => "collection",
        EventTarget::Global => "global",
    };

    let op_str = match event.operation {
        EventOperation::Create => "create",
        EventOperation::Update => "update",
        EventOperation::Delete => "delete",
    };

    // Apply mode: full = after_read hooks + data, metadata = no data. Select by
    // target — a collection and a global may share a slug.
    let mode = match event.target {
        EventTarget::Collection => access.collection_modes.get(slug_str),
        EventTarget::Global => access.global_modes.get(slug_str),
    }
    .copied()
    .unwrap_or_default();

    let data: Map<String, Value> = if mode == LiveMode::Full {
        let (hooks, field_defs) = match event.target {
            EventTarget::Collection => registry
                .get_collection(slug_str)
                .map(|d| (d.hooks.clone(), d.fields.clone())),
            EventTarget::Global => registry
                .get_global(slug_str)
                .map(|d| (d.hooks.clone(), d.fields.clone())),
        }
        .unwrap_or_default();

        let processed_data = hook_runner.apply_after_read_for_event(&EventAfterReadInput {
            collection: slug_str,
            hooks: &hooks,
            fields: &field_defs,
            document_id: event.document_id.as_ref(),
            data: &event.data,
            user: user_doc,
            operation: op_str,
            timestamp: event.timestamp.as_str(),
        });

        let mut visible: Map<String, Value> = processed_data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let denied = match event.target {
            EventTarget::Collection => access.collection_denied_fields.get(slug_str),
            EventTarget::Global => access.global_denied_fields.get(slug_str),
        };
        if let Some(denied) = denied {
            for denial in denied {
                denial.strip_from(&mut visible);
            }
        }

        visible
    } else {
        Map::new() // metadata mode: no data
    };

    // Editor identity is server-side only — leaking the editor's id/email to
    // every subscriber is a PII exposure. Subscribers get a boolean "was this
    // my own edit" computed here instead; editor-based logic belongs in the
    // server-side `live` filter / `before_broadcast` hooks, whose contexts
    // keep the full `edited_by`.
    let is_self = match (&event.edited_by, user_doc) {
        (Some(editor), Some(user)) => editor.id == *user.id,
        _ => false,
    };

    Some(json!({
        "sequence": event.sequence,
        "timestamp": event.timestamp,
        "target": target_str,
        "operation": op_str,
        "collection": event.collection,
        "document_id": event.document_id,
        "self": is_self,
        "data": data,
    }))
}

/// Convert a mutation event to an SSE Event, applying access control, `after_read` hooks,
/// and field stripping to match normal read operations.
pub(super) fn event_to_sse(
    event: &MutationEvent,
    access: &SseAccess,
    hook_runner: &HookRunner,
    registry: &Registry,
    user_doc: Option<&Document>,
) -> Option<Event> {
    let payload = build_event_payload(event, access, hook_runner, registry, user_doc)?;

    Some(
        Event::default()
            .event("mutation")
            .id(event.sequence.to_string())
            .data(payload.to_string()),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            Access, CollectionDefinition, DocumentFields, DocumentId, FieldAccess, FieldDefinition,
            FieldType, Slug,
            event::{EventUser, EventViewMeta},
        },
    };

    /// A `collection_views` map granting the unconstrained published view for one
    /// slug — the common fixture for these payload tests.
    fn published_views(slug: &str) -> HashMap<String, EventViewGate> {
        let mut views = HashMap::new();
        views.insert(
            slug.to_string(),
            EventViewGate {
                published: Some(Vec::new()),
                draft: None,
                trash: None,
            },
        );
        views
    }

    /// Build a posts collection with one field that has field-level read access
    /// denied for everyone. Field stripping in `event_to_sse` (Full mode) must
    /// remove this field per-subscriber before emission.
    fn make_posts_with_secret_field() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.live_mode = LiveMode::Full;
        def.access = Access::default();
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            // A USER-DEFINED field that happens to be named `edited_by` — it
            // lives inside the payload's `data` and goes through the normal
            // field-access pipeline, unlike the removed top-level transport
            // key of the same name.
            FieldDefinition::builder("edited_by", FieldType::Text).build(),
            FieldDefinition {
                name: "secret".to_string(),
                field_type: FieldType::Text,
                access: FieldAccess {
                    read: Some(HookRef::new("hooks.access.field_read_deny")),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        def
    }

    fn make_event(slug: &str, data: DocumentFields) -> MutationEvent {
        MutationEvent {
            sequence: 1,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            target: EventTarget::Collection,
            operation: EventOperation::Create,
            collection: Slug::new(slug),
            document_id: DocumentId::new("doc-1"),
            data,
            edited_by: None,
            view: Some(EventViewMeta::default()),
        }
    }

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
    }

    fn build_runner_and_registry() -> (HookRunner, Arc<Registry>, CollectionDefinition) {
        let config_dir = fixture_dir();
        let config = CrapConfig::test_default();

        // init_lua loads the fixture's collections + hooks into a snapshot Arc.
        let mut registry = crate::hooks::init_lua(&config_dir, &config).expect("init lua");

        // Inject a stripped-down posts collection with the field-level read deny
        // this test needs. `Arc::make_mut` clones if not uniquely owned (no other
        // clones exist yet at this point) and returns &mut Registry.
        Arc::make_mut(&mut registry).register_collection(make_posts_with_secret_field());

        let runner = HookRunner::builder()
            .config_dir(&config_dir)
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .expect("build runner");

        let posts = registry.get_collection("posts").unwrap().clone();

        (runner, registry, posts)
    }

    #[test]
    fn sse_full_mode_strips_field_read_denied_fields() {
        let (runner, registry, _posts) = build_runner_and_registry();

        // Build SseAccess that mirrors what `build_allowed_slugs` would compute
        // for an anonymous user against this posts collection.
        let mut denied_fields: HashMap<String, Vec<FieldDenial>> = HashMap::new();
        denied_fields.insert(
            "posts".to_string(),
            vec![FieldDenial::Flat("secret".into())],
        );

        let mut modes: HashMap<String, LiveMode> = HashMap::new();
        modes.insert("posts".to_string(), LiveMode::Full);

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: denied_fields,
            global_denied_fields: HashMap::new(),
            collection_modes: modes,
            global_modes: HashMap::new(),
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("secret".to_string(), json!("redacted-please"));
        let event = make_event("posts", data);

        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload should be produced for allowed collection");

        let data_obj = payload
            .get("data")
            .and_then(|v| v.as_object())
            .expect("data field should be a JSON object");

        assert_eq!(
            data_obj.get("title"),
            Some(&json!("Hello")),
            "title must be present in Full mode"
        );
        assert!(
            !data_obj.contains_key("secret"),
            "denied field 'secret' must be stripped; got: {data_obj:?}"
        );
    }

    #[test]
    fn sse_metadata_mode_omits_data_entirely() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let mut modes: HashMap<String, LiveMode> = HashMap::new();
        modes.insert("posts".to_string(), LiveMode::Metadata);

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: modes,
            global_modes: HashMap::new(),
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));
        data.insert("secret".to_string(), json!("redacted-please"));
        let event = make_event("posts", data);

        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload yielded");

        let data_obj = payload
            .get("data")
            .and_then(|v| v.as_object())
            .expect("data object present");
        assert!(
            data_obj.is_empty(),
            "metadata mode must emit empty data object; got {data_obj:?}"
        );
    }

    /// Security: the SSE payload must never expose the editing user's
    /// identity (id/email) to subscribers — only the server-computed `self`
    /// boolean, true exactly when the authenticated subscriber IS the editor.
    /// (It previously sent the full `edited_by` `{id, email}` object to every
    /// subscriber.)
    #[test]
    fn sse_payload_exposes_self_flag_but_never_editor_identity() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: HashMap::new(),
            global_modes: HashMap::new(),
        };

        let mut event = make_event("posts", DocumentFields::new());
        event.edited_by = Some(EventUser::new("user-1", "editor@example.com"));

        // Anonymous subscriber → self = false, and no identity anywhere in
        // the serialized payload.
        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload yielded");
        assert_eq!(payload.get("self"), Some(&json!(false)));
        assert!(
            payload.get("edited_by").is_none(),
            "edited_by must not be in the payload: {payload}"
        );
        let raw = payload.to_string();
        assert!(
            !raw.contains("user-1") && !raw.contains("editor@example.com"),
            "payload must not leak editor identity: {raw}"
        );

        // The subscriber IS the editor → self = true.
        let me = Document::new("user-1");
        let payload = build_event_payload(&event, &access, &runner, &registry, Some(&me))
            .expect("payload yielded");
        assert_eq!(payload.get("self"), Some(&json!(true)));

        // A different authenticated subscriber → self = false.
        let other = Document::new("user-2");
        let payload = build_event_payload(&event, &access, &runner, &registry, Some(&other))
            .expect("payload yielded");
        assert_eq!(payload.get("self"), Some(&json!(false)));
    }

    /// A USER-DEFINED field named `edited_by` is ordinary document data: it
    /// must flow through to `payload.data` (subject to field access like any
    /// other field). Only the old top-level transport key of the same name —
    /// the editor-identity leak — is gone.
    #[test]
    fn user_field_named_edited_by_still_flows_through_data() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let mut modes: HashMap<String, LiveMode> = HashMap::new();
        modes.insert("posts".to_string(), LiveMode::Full);

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: modes,
            global_modes: HashMap::new(),
        };

        let mut data = DocumentFields::new();
        data.insert("edited_by".to_string(), json!("a plain document value"));
        let mut event = make_event("posts", data);
        event.edited_by = Some(EventUser::new("user-1", "editor@example.com"));

        let payload = build_event_payload(&event, &access, &runner, &registry, None)
            .expect("payload yielded");

        // The user field is present inside `data`...
        assert_eq!(
            payload.get("data").and_then(|d| d.get("edited_by")),
            Some(&json!("a plain document value")),
            "user-defined edited_by field must pass through: {payload}"
        );
        // ...while the editor's identity still appears nowhere.
        let raw = payload.to_string();
        assert!(
            !raw.contains("user-1") && !raw.contains("editor@example.com"),
            "payload must not leak editor identity: {raw}"
        );
    }

    /// Regression: a collection and a global may share a slug (tables are
    /// namespaced, no cross-uniqueness check). Their delivery `modes` and field
    /// denials must be looked up by event target, not merged into one
    /// slug-keyed map — otherwise one clobbers the other and leaks the full
    /// payload where metadata was configured (or vice-versa). Here the
    /// collection `posts` is Full and the global `posts` is Metadata; each event
    /// must honor its own target's mode.
    #[test]
    fn sse_collection_and_global_sharing_slug_do_not_collide() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let mut collection_modes = HashMap::new();
        collection_modes.insert("posts".to_string(), LiveMode::Full);
        let mut global_modes = HashMap::new();
        global_modes.insert("posts".to_string(), LiveMode::Metadata);

        let access = SseAccess {
            collection_views: published_views("posts"),
            global_views: published_views("posts"),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes,
            global_modes,
        };

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));

        // Collection event → Full → data present.
        let col_event = make_event("posts", data.clone());
        let col_payload = build_event_payload(&col_event, &access, &runner, &registry, None)
            .expect("collection payload");
        assert_eq!(
            col_payload.get("data").and_then(|d| d.get("title")),
            Some(&json!("Hello")),
            "collection 'posts' is Full mode — data must be present: {col_payload}"
        );

        // Global event (same slug) → Metadata → empty data. Pre-fix the merged
        // map would have made the collection Metadata too (global clobbered it).
        let mut global_event = make_event("posts", data);
        global_event.target = EventTarget::Global;
        let global_payload = build_event_payload(&global_event, &access, &runner, &registry, None)
            .expect("global payload");
        assert!(
            global_payload
                .get("data")
                .and_then(|d| d.as_object())
                .expect("data object")
                .is_empty(),
            "global 'posts' is Metadata mode — data must be empty: {global_payload}"
        );
    }

    /// Build an `SseAccess` exposing exactly `gate` for the "posts" collection.
    fn access_with_gate(gate: EventViewGate) -> SseAccess {
        let mut collection_views = HashMap::new();
        collection_views.insert("posts".to_string(), gate);

        SseAccess {
            collection_views,
            global_views: HashMap::new(),
            collection_denied_fields: HashMap::new(),
            global_denied_fields: HashMap::new(),
            collection_modes: HashMap::new(),
            global_modes: HashMap::new(),
        }
    }

    fn event_with_view(status: Option<&str>, trashed: bool) -> MutationEvent {
        let mut event = make_event("posts", DocumentFields::new());
        event.view = Some(EventViewMeta {
            status: status.map(str::to_string),
            trashed,
        });
        event
    }

    /// Regression: a `read`-only subscriber must NOT receive draft mutation
    /// events. Before P4, drafts streamed regardless of the subscriber's view.
    #[test]
    fn draft_event_dropped_when_only_published_visible() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: Some(Vec::new()),
            draft: None,
            trash: None,
        });
        let event = event_with_view(Some("draft"), false);

        assert!(
            build_event_payload(&event, &access, &runner, &registry, None).is_none(),
            "draft event must be withheld from a published-only subscriber"
        );
    }

    /// Fail-closed: an event whose view metadata is absent (e.g. emitted by a
    /// pre-view node during a rolling upgrade) is dropped — never defaulted to
    /// the published view — even for a fully-visible subscriber.
    #[test]
    fn view_less_event_is_dropped() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: Some(Vec::new()),
            draft: Some(Vec::new()),
            trash: Some(Vec::new()),
        });
        let mut event = event_with_view(Some("published"), false);
        event.view = None; // arrived without view metadata

        assert!(
            build_event_payload(&event, &access, &runner, &registry, None).is_none(),
            "an event without view metadata must be dropped (fail-closed)"
        );
    }

    #[test]
    fn draft_event_delivered_when_draft_view_visible() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: None,
            draft: Some(Vec::new()),
            trash: None,
        });
        let event = event_with_view(Some("draft"), false);

        assert!(
            build_event_payload(&event, &access, &runner, &registry, None).is_some(),
            "a draft-visible subscriber receives draft events"
        );
    }

    /// Views are independent: a draft-only reviewer does NOT see published events.
    #[test]
    fn published_event_dropped_when_only_draft_visible() {
        let (runner, registry, _posts) = build_runner_and_registry();

        let access = access_with_gate(EventViewGate {
            published: None,
            draft: Some(Vec::new()),
            trash: None,
        });
        let event = event_with_view(Some("published"), false);

        assert!(build_event_payload(&event, &access, &runner, &registry, None).is_none());
    }

    /// Soft-delete (trashed) events are gated by `trash`, not the status views.
    #[test]
    fn trashed_event_gated_by_trash_view() {
        let (runner, registry, _posts) = build_runner_and_registry();
        let event = event_with_view(Some("published"), true);

        // Published + draft visible but trash denied → withheld.
        let denied_trash = access_with_gate(EventViewGate {
            published: Some(Vec::new()),
            draft: Some(Vec::new()),
            trash: None,
        });
        assert!(
            build_event_payload(&event, &denied_trash, &runner, &registry, None).is_none(),
            "a trashed-doc event must require the trash view"
        );

        // Trash visible → delivered.
        let allow_trash = access_with_gate(EventViewGate {
            published: None,
            draft: None,
            trash: Some(Vec::new()),
        });
        assert!(
            build_event_payload(&event, &allow_trash, &runner, &registry, None).is_some(),
            "a trash-visible subscriber receives soft-delete events"
        );
    }

    /// Regression: a global access hook that returns a filter table is a config
    /// error every synchronous global path rejects. On the SSE stream it must
    /// fail closed (drop the view), never apply a row filter globals don't honor.
    /// Collections, by contrast, keep the constraint as a row filter.
    #[test]
    fn global_constrained_view_is_dropped_collection_is_kept() {
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
