//! Unit tests for the shared live-event delivery gate.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use serde_json::json;

use super::*;
use crate::{
    config::CrapConfig,
    core::{
        Access, EventGateSnapshot, FieldDefinition, FieldType, VersionsConfig, event::EventViewMeta,
    },
    db::{Filter, FilterOp, pool},
    hooks,
};

pub(super) fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hook_tests")
}

/// Resolve a collection `posts` and a global `banner` that share one
/// access shape (drafts enabled, `read` allowing, `draft` = `draft_rule`)
/// and return each one's draft-view filter count (`None` = view hidden).
fn resolve_draft_pair(draft_rule: &str) -> (Option<usize>, Option<usize>) {
    let mut access = Access::new();
    access.read = Some("hooks.access.allow_all".into());
    access.draft = Some(draft_rule.into());

    let mut posts = CollectionDefinition::new("posts");
    posts.versions = Some(VersionsConfig::new(true, 0));
    posts.access = access.clone();

    let mut banner = GlobalDefinition::new("banner");
    banner.versions = Some(VersionsConfig::new(true, 0));
    banner.access = access;

    let mut reg = Registry::new();
    reg.register_collection(posts);
    reg.register_global(banner);
    let registry = Arc::new(reg);

    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let mut db_config = CrapConfig::test_default();
    db_config.database.path = "test.db".to_string();
    let db_pool = pool::create_pool(tmp.path(), &db_config).unwrap();
    let conn = db_pool.get().unwrap();

    let map = EventAccessMap::resolve(&EventAccessInput {
        registry: registry.as_ref(),
        collection_slugs: &["posts".to_string()],
        global_slugs: &["banner".to_string()],
        user_doc: None,
        hook_runner: &runner,
        conn: &conn,
    });

    let draft_len =
        |gate: Option<&EventViewGate>| gate.and_then(|g| g.draft.as_ref().map(Vec::len));

    (
        draft_len(map.collection_views.get("posts")),
        draft_len(map.global_views.get("banner")),
    )
}

/// Regression: the global branch hard-coded `draft: None`, so a global
/// with drafts never delivered its draft events even to a subscriber its
/// `access.draft` rule allowed — while a collection with the identical
/// access shape did. Both must resolve the draft axis identically.
#[test]
fn draft_axis_resolves_identically_for_collections_and_globals() {
    let allowed = resolve_draft_pair("hooks.access.allow_all");
    assert_eq!(
        allowed,
        (Some(0), Some(0)),
        "an allowing draft rule opens the (unconstrained) draft view on both"
    );

    let denied = resolve_draft_pair("hooks.access.deny_all");
    assert_eq!(
        denied,
        (None, None),
        "a denying draft rule hides the draft view on both"
    );
}

/// A global draft event is delivered exactly when the resolved draft view
/// is visible — the gate reads the same axis the resolution now fills.
#[test]
fn global_draft_event_is_gated_by_the_draft_view() {
    let draft_event = EventViewMeta {
        status: Some("draft".to_string()),
        ..EventViewMeta::default()
    };

    let open = EventViewGate {
        published: Some(vec![]),
        draft: Some(vec![]),
        trash: None,
    };
    assert!(open.constraints_for(&draft_event).is_some());

    let closed = EventViewGate {
        published: Some(vec![]),
        draft: None,
        trash: None,
    };
    assert!(closed.constraints_for(&draft_event).is_none());
}

/// What `user` receives, subscribed unconstrained to the `Full`-mode
/// `event_leak` collection, for an update event whose document is `data`
/// — the stored row, stripped for no one.
fn leak_delivery(user: Option<&Document>, data: DocumentFields) -> Map<String, Value> {
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let registry = hooks::init_lua(&config_dir, &config).unwrap();
    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let event = MutationEvent {
        sequence: 1,
        publisher: String::new(),
        timestamp: "2026-08-11T00:00:00Z".to_string(),
        target: EventTarget::Collection,
        operation: EventOperation::Update,
        collection: "event_leak".into(),
        document_id: "d1".into(),
        data,
        edited_by: None,
        view: Some(EventViewMeta::default()),
        gate: None,
    };

    let views = HashMap::from([(
        "event_leak".to_string(),
        EventViewGate {
            published: Some(vec![]),
            draft: None,
            trash: None,
        },
    )]);
    let modes = HashMap::from([("event_leak".to_string(), LiveMode::Full)]);
    let empty_views = HashMap::new();
    let empty_modes = HashMap::new();

    let gate = EventGate {
        collection_views: &views,
        global_views: &empty_views,
        collection_modes: &modes,
        global_modes: &empty_modes,
        registry: &registry,
        hook_runner: &runner,
        user_doc: user,
    };

    gate.evaluate(&event).expect("event must be delivered").data
}

/// The stored `event_leak` row: every field set, `secret` (read-denied to
/// everyone), `notes` (readable by admins only) and `internal` (hidden).
fn leak_row() -> DocumentFields {
    DocumentFields::from(HashMap::from([
        ("title".to_string(), json!("Hello")),
        ("secret".to_string(), json!("s3cr3t-value")),
        ("notes".to_string(), json!("admin-notes")),
        ("internal".to_string(), json!("internal-value")),
    ]))
}

fn user_with_role(role: &str) -> Document {
    let mut user = Document::new("u1");
    user.fields.insert("role".to_string(), json!(role));
    user
}

/// Regression: the Full-mode event pipeline ran per-subscriber
/// `after_read` hooks BEFORE the field-read strip (normal reads strip
/// first). A hook copying a read-denied field's value into an
/// unprotected field leaked it past the strip to a denied subscriber.
#[test]
fn full_payload_strips_before_after_read() {
    let visible = leak_delivery(None, leak_row());

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

/// Regression: a `Full`-mode event delivered the document as stripped for
/// the WRITER, so a subscriber allowed a field the writer was denied never
/// received it. Delivery starts from the stored row: each subscriber gets
/// exactly what its own read would — a field it may read is delivered, one
/// it may not is stripped, a hidden one never travels to anyone.
#[test]
fn full_payload_is_stripped_by_each_subscribers_own_access() {
    let admin = user_with_role("admin");
    let editor = user_with_role("editor");

    let for_admin = leak_delivery(Some(&admin), leak_row());
    assert_eq!(for_admin.get("notes"), Some(&json!("admin-notes")));
    assert_eq!(for_admin.get("title"), Some(&json!("Hello")));

    for user in [Some(&editor), None] {
        let delivered = leak_delivery(user, leak_row());

        assert!(delivered.get("notes").is_none(), "{delivered:?}");
        assert_eq!(delivered.get("title"), Some(&json!("Hello")));
    }

    for user in [Some(&admin), Some(&editor), None] {
        let delivered = leak_delivery(user, leak_row());
        let printed = Value::Object(delivered.clone()).to_string();

        assert!(delivered.get("internal").is_none(), "{printed}");
        assert!(delivered.get("secret").is_none(), "{printed}");
        assert!(!printed.contains("internal-value"), "{printed}");
        assert!(!printed.contains("s3cr3t-value"), "{printed}");
    }
}

/// Regression: a global access hook that returns a filter table is a config
/// error every synchronous global path rejects. On the live streams it must
/// fail closed (drop the view), never apply a row filter globals don't honor.
/// Collections, by contrast, keep the constraint as a row filter.
#[test]
fn global_constrained_view_is_dropped_collection_is_kept() {
    let filters = vec![FilterClause::and(Vec::new())];

    // Global (reject_constrained = true): filter table → hidden.
    assert!(
        view_from_access(AccessResult::Constrained(filters.clone()), true, "settings").is_none(),
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

/// A `secrets` collection with a has-many `tags` list and a `flag` text
/// field, delivered in `mode`.
fn secrets_registry(mode: LiveMode) -> Arc<Registry> {
    let mut secrets = CollectionDefinition::new("secrets");
    secrets.live_mode = mode;
    secrets.fields = vec![
        FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build(),
        FieldDefinition::builder("flag", FieldType::Text).build(),
    ];

    let mut reg = Registry::new();
    reg.register_collection(secrets);

    Arc::new(reg)
}

/// What a subscriber whose published view of `secrets` carries
/// `constraints` receives for `event` (`None` = dropped).
fn deliver(
    mode: LiveMode,
    event: &MutationEvent,
    constraints: Vec<FilterClause>,
) -> Option<Map<String, Value>> {
    let registry = secrets_registry(mode);
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let views = HashMap::from([(
        "secrets".to_string(),
        EventViewGate {
            published: Some(constraints),
            draft: None,
            trash: None,
        },
    )]);
    let modes = HashMap::from([("secrets".to_string(), mode)]);
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

    gate.evaluate(event).map(|delivery| delivery.data)
}

/// Whether a subscriber constrained by `constraint` receives `event`.
fn constrained_delivery(mode: LiveMode, event: &MutationEvent, constraint: FilterClause) -> bool {
    deliver(mode, event, vec![constraint]).is_some()
}

/// An event for `secrets/d1` delivering `data`, gated by `row` (the stored
/// row; `None` = no snapshot).
fn secrets_event(
    operation: EventOperation,
    data: DocumentFields,
    row: Option<DocumentFields>,
) -> MutationEvent {
    let gate = row.map(|fields| {
        let mut doc = Document::new("d1");
        doc.fields = fields;

        EventGateSnapshot::of(&doc)
    });

    MutationEvent {
        sequence: 1,
        publisher: String::new(),
        timestamp: "2026-09-23T00:00:00Z".to_string(),
        target: EventTarget::Collection,
        operation,
        collection: "secrets".into(),
        document_id: "d1".into(),
        data,
        edited_by: None,
        view: Some(EventViewMeta::default()),
        gate,
    }
}

fn constraint(field: &str, op: FilterOp) -> FilterClause {
    FilterClause::Single(Filter {
        field: field.to_string(),
        op,
    })
}

fn not_in_secret() -> FilterClause {
    constraint("tags", FilterOp::NotIn(vec!["secret".to_string()]))
}

fn flag_is_set() -> FilterClause {
    constraint("flag", FilterOp::Equals("set".to_string()))
}

fn flag_not_exists() -> FilterClause {
    constraint("flag", FilterOp::NotExists)
}

/// A stored `secrets` row: `tags` as given, `flag` set or NULL.
fn document(tags: Value, flag: Option<&str>) -> DocumentFields {
    DocumentFields::from(HashMap::from([
        ("tags".to_string(), tags),
        ("flag".to_string(), flag.map_or(Value::Null, |f| json!(f))),
    ]))
}

/// The operations a payload-less event can carry in `Metadata` mode.
const OPERATIONS: [EventOperation; 3] = [
    EventOperation::Create,
    EventOperation::Update,
    EventOperation::Delete,
];

/// Regression: a `Metadata`-mode event carries no data, so a constrained
/// subscriber could never be told of a change to a document it can see.
/// It is judged by the stored row it carries: delivered (metadata only)
/// exactly when the row satisfies the constraint.
#[test]
fn metadata_mode_event_reaches_a_constrained_view_iff_the_row_matches() {
    let public = document(json!(["public"]), None);
    let secret = document(json!(["secret"]), Some("set"));

    for operation in OPERATIONS {
        let visible = secrets_event(
            operation.clone(),
            DocumentFields::new(),
            Some(public.clone()),
        );
        let hidden = secrets_event(
            operation.clone(),
            DocumentFields::new(),
            Some(secret.clone()),
        );

        assert_eq!(
            deliver(LiveMode::Metadata, &visible, vec![not_in_secret()]),
            Some(Map::new()),
            "{operation:?}: a matching row is delivered, metadata only"
        );
        assert!(constrained_delivery(
            LiveMode::Metadata,
            &visible,
            flag_not_exists()
        ));
        assert!(!constrained_delivery(
            LiveMode::Metadata,
            &visible,
            flag_is_set()
        ));

        assert!(!constrained_delivery(
            LiveMode::Metadata,
            &hidden,
            not_in_secret()
        ));
        assert!(!constrained_delivery(
            LiveMode::Metadata,
            &hidden,
            flag_not_exists()
        ));
        assert!(constrained_delivery(
            LiveMode::Metadata,
            &hidden,
            flag_is_set()
        ));
    }
}

/// Regression: a delete carries no document, so a constrained subscriber
/// never learned that a document it could see was deleted. In `Full` mode
/// too, the removed row decides.
#[test]
fn full_mode_delete_reaches_a_constrained_view_iff_the_row_matches() {
    let visible = secrets_event(
        EventOperation::Delete,
        DocumentFields::new(),
        Some(document(json!(["public"]), None)),
    );
    let hidden = secrets_event(
        EventOperation::Delete,
        DocumentFields::new(),
        Some(document(json!(["secret"]), Some("set"))),
    );

    assert!(constrained_delivery(
        LiveMode::Full,
        &visible,
        not_in_secret()
    ));
    assert!(constrained_delivery(
        LiveMode::Full,
        &visible,
        flag_not_exists()
    ));
    assert!(!constrained_delivery(
        LiveMode::Full,
        &hidden,
        not_in_secret()
    ));
    assert!(!constrained_delivery(
        LiveMode::Full,
        &hidden,
        flag_not_exists()
    ));
    assert!(constrained_delivery(LiveMode::Full, &hidden, flag_is_set()));
}

/// An event without a snapshot — from a node that predates it, or one
/// dropped to fit the transport cap — cannot be judged: a constrained view
/// never receives it, whatever the operators, negative ones included.
#[test]
fn event_without_a_snapshot_never_reaches_a_constrained_view() {
    for mode in [LiveMode::Metadata, LiveMode::Full] {
        for operation in OPERATIONS {
            let event = secrets_event(operation, document(json!(["public"]), None), None);

            assert!(!constrained_delivery(mode, &event, not_in_secret()));
            assert!(!constrained_delivery(mode, &event, flag_not_exists()));
        }
    }
}

/// A `Full`-mode event is judged by its stored row, not by the delivered
/// payload (which `before_broadcast` may reshape): a payload claiming a
/// public row cannot smuggle out a secret one, and a stripped payload does
/// not hide a visible one.
#[test]
fn full_mode_event_is_judged_by_the_row_not_the_payload() {
    let disguised = secrets_event(
        EventOperation::Update,
        document(json!(["public"]), None),
        Some(document(json!(["secret"]), None)),
    );
    let reshaped = secrets_event(
        EventOperation::Update,
        DocumentFields::new(),
        Some(document(json!(["public"]), None)),
    );

    assert!(!constrained_delivery(
        LiveMode::Full,
        &disguised,
        not_in_secret()
    ));
    assert!(constrained_delivery(
        LiveMode::Full,
        &reshaped,
        not_in_secret()
    ));
}

/// The snapshot is used for gating and never delivered: a field only the
/// stored row carries reaches no subscriber in either mode.
#[test]
fn snapshot_is_never_delivered() {
    let mut row = document(json!(["public"]), Some("s3cr3t-sentinel"));
    row.insert("hidden_owner".to_string(), json!("s3cr3t-sentinel"));

    let mut payload = DocumentFields::new();
    payload.insert("tags".to_string(), json!(["public"]));

    let event = secrets_event(EventOperation::Update, payload, Some(row));

    let full = deliver(LiveMode::Full, &event, vec![not_in_secret()]).expect("delivered");
    assert!(
        !Value::Object(full.clone())
            .to_string()
            .contains("s3cr3t-sentinel"),
        "{full:?}"
    );
    assert!(full.get("hidden_owner").is_none());

    let metadata = deliver(LiveMode::Metadata, &event, vec![not_in_secret()]);
    assert_eq!(metadata, Some(Map::new()));
}

/// An unconstrained view still receives metadata-only events and deletes,
/// with or without a snapshot.
#[test]
fn unconstrained_view_receives_payloadless_events() {
    for operation in [EventOperation::Update, EventOperation::Delete] {
        for row in [None, Some(document(json!(["secret"]), None))] {
            let event = secrets_event(operation.clone(), DocumentFields::new(), row);

            assert_eq!(
                deliver(LiveMode::Metadata, &event, Vec::new()),
                Some(Map::new())
            );
        }
    }
}
