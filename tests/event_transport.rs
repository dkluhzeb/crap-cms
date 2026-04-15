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

use std::{collections::HashMap, sync::Arc};

use crap_cms::config::LiveConfig;
use crap_cms::core::{
    DocumentId, Slug,
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
        data: HashMap::new(),
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
        other => panic!("expected Lagged, got {:?}", other),
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

#[test]
fn factory_rejects_unknown_transport() {
    let cfg = LiveConfig {
        transport: "kafka".to_string(),
        ..LiveConfig::default()
    };

    let Err(err) = create_event_transport(&cfg, "") else {
        panic!("expected error for unknown transport");
    };
    assert!(
        err.to_string().contains("kafka"),
        "unexpected error: {}",
        err
    );
}

#[cfg(not(feature = "redis"))]
#[test]
fn factory_rejects_redis_without_feature() {
    let cfg = LiveConfig {
        transport: "redis".to_string(),
        ..LiveConfig::default()
    };

    let Err(err) = create_event_transport(&cfg, "redis://localhost") else {
        panic!("expected error when redis feature is disabled");
    };
    assert!(
        err.to_string().contains("redis` feature"),
        "unexpected error: {}",
        err
    );
}
