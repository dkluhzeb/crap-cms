//! Event transport abstraction integration tests.
//!
//! Verifies that the in-process transport satisfies the `EventTransport` /
//! `InvalidationTransport` contract end-to-end (publish -> receive, fanout,
//! lagged signalling) and that the `create_event_transport` factory wires up
//! the correct backend from config.
//!
//! The Redis transport cannot be exercised here without a running Redis; it
//! is covered by unit-level wire-format tests inside
//! `src/core/event/redis_transport.rs`. Real-world operators are expected to
//! smoke-test Redis fanout against their deployment.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::sync::Arc;

use crap_cms::config::LiveConfig;
#[cfg(not(feature = "redis"))]
use crap_cms::config::LiveTransport;
use crap_cms::core::{
    DocumentFields, DocumentId, Slug,
    event::{
        EventOperation, EventTarget, InProcessEventBus, InProcessInvalidationBus,
        InvalidationTransport, MutationEventInput, RecvError, SharedEventTransport,
        create_event_transport, create_invalidation_transport,
    },
};

fn sample_input() -> MutationEventInput {
    MutationEventInput {
        target: EventTarget::Collection,
        operation: EventOperation::Create,
        collection: Slug::new("posts"),
        document_id: DocumentId::new("doc-1"),
        data: DocumentFields::new(),
        edited_by: None,
    }
}

#[tokio::test]
async fn in_process_event_transport_roundtrip() {
    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
    let mut rx = transport.subscribe();

    transport.publish(sample_input());

    let ev = rx.recv().await.expect("receive event");
    assert_eq!(ev.collection, "posts");
    assert_eq!(ev.operation, EventOperation::Create);
}

#[tokio::test]
async fn in_process_event_transport_fanout_to_multiple_subscribers() {
    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
    let mut rx1 = transport.subscribe();
    let mut rx2 = transport.subscribe();

    transport.publish(sample_input());

    let a = rx1.recv().await.unwrap();
    let b = rx2.recv().await.unwrap();
    assert_eq!(a.sequence, b.sequence);
    assert_eq!(a.document_id, "doc-1");
}

#[tokio::test]
async fn in_process_event_transport_lagged_error_surfaces() {
    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(2));
    let mut rx = transport.subscribe();

    for _ in 0..5 {
        transport.publish(sample_input());
    }

    // Next recv must surface a Lagged, matching the broadcast channel semantic.
    match rx.recv().await {
        Err(RecvError::Lagged(n)) => assert!(n >= 1),
        other => panic!("expected Lagged, got {other:?}"),
    }
}

#[tokio::test]
async fn in_process_invalidation_transport_roundtrip() {
    let transport = InProcessInvalidationBus::new();
    let mut rx = transport.subscribe();

    transport.publish("user-42".to_string());

    let id = rx.recv().await.expect("receive invalidation id");
    assert_eq!(id, "user-42");
}

#[test]
fn factory_defaults_to_memory_transport() {
    let cfg = LiveConfig::default();
    let transport = create_event_transport(&cfg, "redis://127.0.0.1:6379")
        .expect("factory ok")
        .expect("live enabled -> Some transport");
    assert_eq!(transport.kind(), "in_process");

    let inv = create_invalidation_transport(&cfg, "redis://127.0.0.1:6379").expect("factory ok");
    assert_eq!(inv.kind(), "in_process");
}

#[test]
fn factory_honours_disabled_live() {
    let cfg = LiveConfig {
        enabled: false,
        ..LiveConfig::default()
    };

    let transport = create_event_transport(&cfg, "").expect("factory ok");
    assert!(transport.is_none());
}

#[cfg(not(feature = "redis"))]
#[test]
fn factory_rejects_redis_without_feature() {
    let cfg = LiveConfig {
        transport: LiveTransport::Redis,
        ..LiveConfig::default()
    };

    let Err(err) = create_event_transport(&cfg, "redis://localhost") else {
        panic!("expected error when redis feature is disabled");
    };
    assert!(
        err.to_string().contains("redis` feature"),
        "unexpected error: {err}"
    );
}

// ── Service-layer event emission ────────────────────────────────────────

/// Regression: `unpublish_global_document` must emit a mutation event after
/// commit, exactly like `update_global_document` and the collection unpublish
/// path. It used to commit silently — subscribers never learned a global was
/// unpublished, and the global cache was never cleared.
#[tokio::test]
async fn global_unpublish_emits_mutation_event() {
    use crap_cms::config::CrapConfig;
    use crap_cms::hooks::{self, HookRunner};
    use crap_cms::service::{ServiceContext, unpublish_global_document};

    let tmp = tempfile::tempdir().expect("tempdir");
    let globals_dir = tmp.path().join("globals");
    std::fs::create_dir_all(&globals_dir).unwrap();
    std::fs::write(
        globals_dir.join("site.lua"),
        r#"
crap.globals.define("site", {
    versions = true,
    fields = {
        { name = "title", type = "text" },
    },
})
"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("init.lua"), "").unwrap();

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    let registry = hooks::init_lua(tmp.path(), &config).expect("init_lua");
    let pool = crap_cms::db::pool::create_pool(tmp.path(), &config).expect("pool");
    crap_cms::db::migrate::sync_all(&pool, &registry, &config.locale).expect("sync");
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
    let mut rx = transport.subscribe();

    let def = registry.get_global("site").expect("global def");
    let ctx = ServiceContext::global("site", def)
        .pool(&pool)
        .runner(&runner)
        .event_transport(Some(transport.clone()))
        .build();

    unpublish_global_document(&ctx).expect("unpublish global");

    let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("no mutation event emitted within 5s of global unpublish")
        .expect("mutation event after global unpublish");
    assert_eq!(ev.target, EventTarget::Global);
    assert_eq!(ev.operation, EventOperation::Update);
    assert_eq!(ev.collection.as_ref(), "site");
}
