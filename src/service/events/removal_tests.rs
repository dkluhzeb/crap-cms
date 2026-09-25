//! Unit tests for the removal a move between content views announces.

use std::{collections::HashMap, sync::Arc};

use serde_json::json;

use super::{tests::fixture_dir, *};
use crate::{
    config::CrapConfig,
    core::{
        EventGateSnapshot, EventViewPlacement, FieldDefinition, FieldType, VersionsConfig,
        event::EventViewMeta,
    },
    db::{Filter, FilterOp},
};

/// A drafts-enabled, soft-deleting `posts` collection and a drafts-enabled
/// `notice` global, both with a `title` field, delivered in `mode`.
fn drafts_registry(mode: LiveMode) -> Arc<Registry> {
    let title = || FieldDefinition::builder("title", FieldType::Text).build();

    let mut posts = CollectionDefinition::new("posts");
    posts.live_mode = mode;
    posts.versions = Some(VersionsConfig::new(true, 0));
    posts.soft_delete = true;
    posts.fields = vec![title()];

    let mut notice = GlobalDefinition::new("notice");
    notice.live_mode = mode;
    notice.versions = Some(VersionsConfig::new(true, 0));
    notice.fields = vec![title()];

    let mut reg = Registry::new();
    reg.register_collection(posts);
    reg.register_global(notice);

    Arc::new(reg)
}

fn placed(status: &str, trashed: bool) -> EventViewPlacement {
    EventViewPlacement {
        status: Some(status.to_string()),
        trashed,
    }
}

/// An `operation` event for `target`'s document, which the write left at
/// `now` having moved it from `from`.
fn moved_event(
    target: EventTarget,
    operation: EventOperation,
    now: EventViewPlacement,
    from: EventViewPlacement,
) -> MutationEvent {
    let (collection, id) = match target {
        EventTarget::Collection => ("posts", "d1"),
        EventTarget::Global => ("notice", "default"),
    };

    let mut row = Document::new(id);
    row.fields.insert("title".to_string(), json!("Moved"));
    row.fields
        .insert("_status".to_string(), json!(now.status.clone()));

    if now.trashed {
        row.fields
            .insert("_deleted_at".to_string(), json!("2026-09-25T00:00:00Z"));
    }

    MutationEvent {
        sequence: 1,
        publisher: String::new(),
        timestamp: "2026-09-25T00:00:00Z".to_string(),
        target,
        operation,
        collection: collection.into(),
        document_id: id.into(),
        data: row.fields.clone(),
        edited_by: None,
        view: Some(EventViewMeta::at(now).moved_from(Some(from))),
        gate: Some(EventGateSnapshot::of(&row)),
    }
}

/// An unpublish of `target`'s document: the stored row is now a draft, and
/// `was_published` says whether it was published going in.
fn unpublish_event(target: EventTarget, was_published: bool) -> MutationEvent {
    let from = placed(if was_published { "published" } else { "draft" }, false);

    moved_event(
        target,
        EventOperation::Unpublish,
        placed("draft", false),
        from,
    )
}

/// A soft delete of `posts/d1`: the row now sits in the trash with `status`,
/// having left that status view.
fn trash_event(status: &str) -> MutationEvent {
    let mut event = moved_event(
        EventTarget::Collection,
        EventOperation::Delete,
        placed(status, true),
        placed(status, false),
    );
    event.data = DocumentFields::new();

    event
}

/// What a subscriber with `views` on the event's slug receives for it.
fn deliver_to(
    mode: LiveMode,
    event: &MutationEvent,
    views: EventViewGate,
) -> Option<EventDelivery> {
    let registry = drafts_registry(mode);
    let config_dir = fixture_dir();
    let config = CrapConfig::test_default();
    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();

    let slug = event.collection.to_string();
    let views = HashMap::from([(slug.clone(), views)]);
    let modes = HashMap::from([(slug, mode)]);
    let empty_views = HashMap::new();
    let empty_modes = HashMap::new();

    let (collection_views, global_views, collection_modes, global_modes) = match event.target {
        EventTarget::Collection => (&views, &empty_views, &modes, &empty_modes),
        EventTarget::Global => (&empty_views, &views, &empty_modes, &modes),
    };

    EventGate {
        collection_views,
        global_views,
        collection_modes,
        global_modes,
        registry: &registry,
        hook_runner: &runner,
        user_doc: None,
    }
    .evaluate(event)
}

/// A subscriber's views, each either visible without a constraint or hidden.
fn views(published: bool, draft: bool, trash: bool) -> EventViewGate {
    let open = |visible: bool| visible.then(Vec::new);

    EventViewGate {
        published: open(published),
        draft: open(draft),
        trash: open(trash),
    }
}

fn published_only(constraints: Vec<FilterClause>) -> EventViewGate {
    EventViewGate {
        published: Some(constraints),
        draft: None,
        trash: None,
    }
}

// ── Unpublish ───────────────────────────────────────────────────────────────

/// Regression: an unpublish is gated by the draft view the row moves into,
/// so a subscriber that could see the document only published was never told
/// it left — its client kept showing a document every read of its own now
/// hides. It receives a `delete`, carrying no data, in either mode.
#[test]
fn unpublishing_a_document_is_a_delete_for_published_only_subscribers() {
    for mode in [LiveMode::Full, LiveMode::Metadata] {
        let event = unpublish_event(EventTarget::Collection, true);

        let delivery = deliver_to(mode, &event, published_only(Vec::new()))
            .unwrap_or_else(|| panic!("{mode:?}: the removal must be delivered"));

        assert_eq!(delivery.operation, EventOperation::Delete, "{mode:?}");
        assert!(delivery.data.is_empty(), "{mode:?}: {:?}", delivery.data);
    }
}

/// A subscriber that can see drafts still receives the unpublish itself, with
/// the document in `Full` mode.
#[test]
fn draft_subscribers_still_receive_the_unpublish() {
    let event = unpublish_event(EventTarget::Collection, true);

    let delivery = deliver_to(LiveMode::Full, &event, views(true, true, false)).expect("delivered");

    assert_eq!(delivery.operation, EventOperation::Unpublish);
    assert_eq!(delivery.data.get("title"), Some(&json!("Moved")));
}

/// Regression, global twin: a published-only subscriber receives an `update`
/// carrying the empty global its own read now returns — never the unpublished
/// content.
#[test]
fn unpublishing_a_global_is_an_empty_update_for_published_only_subscribers() {
    let event = unpublish_event(EventTarget::Global, true);

    let delivery =
        deliver_to(LiveMode::Full, &event, published_only(Vec::new())).expect("delivered");

    assert_eq!(delivery.operation, EventOperation::Update);
    assert_eq!(delivery.data.get("title"), None, "{:?}", delivery.data);
    assert_eq!(delivery.data.get("_status"), Some(&json!("draft")));

    let with_drafts =
        deliver_to(LiveMode::Full, &event, views(true, true, false)).expect("delivered");
    assert_eq!(with_drafts.operation, EventOperation::Unpublish);
    assert_eq!(with_drafts.data.get("title"), Some(&json!("Moved")));
}

/// No removal is announced for a row that was not published going in (it
/// never was in the subscriber's view — announcing it would reveal that a
/// draft exists), nor to a subscriber whose published-view constraint the row
/// does not satisfy.
#[test]
fn a_removal_needs_a_row_the_subscriber_saw_published() {
    let already_draft = unpublish_event(EventTarget::Collection, false);
    assert_eq!(
        deliver_to(
            LiveMode::Metadata,
            &already_draft,
            published_only(Vec::new())
        ),
        None
    );

    let left = unpublish_event(EventTarget::Collection, true);
    let title_is = |title: &str| {
        vec![FilterClause::Single(Filter {
            field: "title".to_string(),
            op: FilterOp::Equals(title.to_string()),
        })]
    };

    assert!(deliver_to(LiveMode::Metadata, &left, published_only(title_is("Moved"))).is_some());
    assert_eq!(
        deliver_to(LiveMode::Metadata, &left, published_only(title_is("Other"))),
        None
    );
}

/// A subscriber scoped to some operations wants a removal-carrying event when
/// either the event's own operation or its removal is among them.
#[test]
fn delivered_operations_include_the_removal() {
    let left = unpublish_event(EventTarget::Collection, true);
    assert_eq!(
        delivered_operations(&left),
        vec![EventOperation::Unpublish, EventOperation::Delete]
    );

    let global = unpublish_event(EventTarget::Global, true);
    assert_eq!(
        delivered_operations(&global),
        vec![EventOperation::Unpublish, EventOperation::Update]
    );

    let stayed = unpublish_event(EventTarget::Collection, false);
    assert_eq!(
        delivered_operations(&stayed),
        vec![EventOperation::Unpublish]
    );
}

/// An event from a node that predates the prior placement carries only the
/// legacy flag, and is still a removal for a published-only subscriber.
#[test]
fn a_legacy_left_published_event_is_still_a_removal() {
    let mut event = unpublish_event(EventTarget::Collection, false);
    event.view = Some(EventViewMeta {
        left_published: true,
        ..EventViewMeta::at(placed("draft", false))
    });

    let delivery = deliver_to(LiveMode::Metadata, &event, published_only(Vec::new()))
        .expect("the legacy removal is delivered");

    assert_eq!(delivery.operation, EventOperation::Delete);
}

// ── Soft delete ─────────────────────────────────────────────────────────────

/// Regression: a soft delete is gated by the trash view the row moves into,
/// so a subscriber that could see the document only published was never told
/// it went — its client kept showing a document every read of its own now
/// hides. It receives the `delete`; a trash subscriber receives it as before.
#[test]
fn trashing_a_published_document_is_a_delete_for_published_only_subscribers() {
    let event = trash_event("published");

    for mode in [LiveMode::Full, LiveMode::Metadata] {
        let delivery = deliver_to(mode, &event, published_only(Vec::new()))
            .unwrap_or_else(|| panic!("{mode:?}: the removal must be delivered"));

        assert_eq!(delivery.operation, EventOperation::Delete, "{mode:?}");
        assert!(delivery.data.is_empty(), "{mode:?}: {:?}", delivery.data);
    }

    let trash = deliver_to(LiveMode::Full, &event, views(true, false, true)).expect("delivered");
    assert_eq!(trash.operation, EventOperation::Delete);
}

/// Regression: trashing a draft was announced only to trash subscribers, so
/// a subscriber that could see drafts but not the trash kept showing it. It
/// receives the `delete` — whether or not it can also see published rows.
#[test]
fn trashing_a_draft_is_a_delete_for_draft_subscribers_without_trash() {
    let event = trash_event("draft");

    for gate in [views(true, true, false), views(false, true, false)] {
        let delivery = deliver_to(LiveMode::Full, &event, gate)
            .expect("the draft-view subscriber is told of the removal");

        assert_eq!(delivery.operation, EventOperation::Delete);
        assert!(delivery.data.is_empty(), "{:?}", delivery.data);
    }
}

/// Trashing a draft announces nothing to a published-only subscriber: the
/// row never was in its view, and its id must not reach it.
#[test]
fn trashing_a_draft_stays_hidden_from_published_only_subscribers() {
    let event = trash_event("draft");

    assert_eq!(
        deliver_to(LiveMode::Metadata, &event, published_only(Vec::new())),
        None
    );
}

/// A trash event is a `delete` either way, so its operations list it once.
#[test]
fn delivered_operations_list_a_trash_removal_once() {
    for status in ["published", "draft"] {
        assert_eq!(
            delivered_operations(&trash_event(status)),
            vec![EventOperation::Delete],
            "{status}"
        );
    }
}

// ── Publish and undelete ────────────────────────────────────────────────────

/// Publishing a draft moves the row out of the draft view: a subscriber that
/// can see only drafts is told of the removal, a published-view subscriber
/// receives the update itself.
#[test]
fn publishing_a_draft_is_a_delete_for_draft_only_subscribers() {
    let event = moved_event(
        EventTarget::Collection,
        EventOperation::Update,
        placed("published", false),
        placed("draft", false),
    );

    let draft_only = deliver_to(LiveMode::Metadata, &event, views(false, true, false))
        .expect("the draft-only subscriber is told of the removal");
    assert_eq!(draft_only.operation, EventOperation::Delete);

    let published =
        deliver_to(LiveMode::Metadata, &event, views(true, false, false)).expect("delivered");
    assert_eq!(published.operation, EventOperation::Update);
}

/// A global that leaves its draft view is still there to read: publishing it
/// announces no removal to a draft-only subscriber.
#[test]
fn publishing_a_global_announces_no_removal() {
    let event = moved_event(
        EventTarget::Global,
        EventOperation::Update,
        placed("published", false),
        placed("draft", false),
    );

    assert_eq!(
        deliver_to(LiveMode::Metadata, &event, views(false, true, false)),
        None
    );
    assert_eq!(delivered_operations(&event), vec![EventOperation::Update]);
}

/// Restoring a document from the trash moves it out of the trash view: a
/// trash-only subscriber is told of the removal, a published-view subscriber
/// receives the undelete itself.
#[test]
fn undeleting_is_a_delete_for_trash_only_subscribers() {
    let event = moved_event(
        EventTarget::Collection,
        EventOperation::Undelete,
        placed("published", false),
        placed("published", true),
    );

    let trash_only = deliver_to(LiveMode::Metadata, &event, views(false, false, true))
        .expect("the trash-only subscriber is told of the removal");
    assert_eq!(trash_only.operation, EventOperation::Delete);

    let published =
        deliver_to(LiveMode::Metadata, &event, views(true, false, false)).expect("delivered");
    assert_eq!(published.operation, EventOperation::Undelete);
}
