//! Monotonic sequence generator + event-stamping helper shared between transports.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use nanoid::nanoid;

use super::types::{MutationEvent, MutationEventInput};

/// Monotonic sequence generator shared between transports. Starts at 1.
///
/// Each generator carries a random publisher id: several nodes can publish on
/// one shared transport, each counting from 1, and only the
/// `(publisher, sequence)` pair is unique and gap-free.
#[derive(Clone)]
pub(crate) struct SequenceGen {
    counter: Arc<AtomicU64>,
    publisher: Arc<str>,
}

impl SequenceGen {
    pub(crate) fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(1)),
            publisher: Arc::from(nanoid!(12)),
        }
    }

    pub(crate) fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::AcqRel)
    }

    /// Stamp `input` with this generator's publisher id, its next sequence
    /// number, and the current time.
    pub(crate) fn stamp(&self, input: MutationEventInput) -> MutationEvent {
        stamp_event(input, self.next(), &self.publisher)
    }
}

/// Build a [`MutationEvent`] from an input plus a sequence number, the
/// publisher it belongs to, and the current timestamp.
pub(crate) fn stamp_event(
    input: MutationEventInput,
    sequence: u64,
    publisher: &str,
) -> MutationEvent {
    let MutationEventInput {
        target,
        operation,
        collection,
        document_id,
        data,
        edited_by,
        view,
    } = input;

    MutationEvent {
        sequence,
        publisher: publisher.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        target,
        operation,
        collection,
        document_id,
        data,
        edited_by,
        // Producers always carry view metadata; `Some` marks it as present so a
        // consumer can distinguish it from an event that arrived without one.
        view: Some(view),
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::core::{
        DocumentFields, DocumentId, Slug,
        event::types::{EventOperation, EventTarget},
    };

    /// Separate generators (separate nodes) get distinct publisher ids; clones
    /// of one generator share its id and its counter.
    #[test]
    fn each_generator_has_its_own_publisher() {
        let node_a = SequenceGen::new();
        let node_b = SequenceGen::new();
        let node_a_clone = node_a.clone();

        assert_ne!(node_a.publisher, node_b.publisher);
        assert_eq!(node_a.publisher, node_a_clone.publisher);

        assert_eq!(node_a.next(), 1);
        assert_eq!(node_b.next(), 1, "every publisher counts from 1");
        assert_eq!(node_a_clone.next(), 2);
    }

    #[test]
    fn sequence_gen_is_monotonic() {
        let seq = SequenceGen::new();
        assert_eq!(seq.next(), 1);
        assert_eq!(seq.next(), 2);
        assert_eq!(seq.next(), 3);
    }

    #[test]
    fn stamp_event_fills_sequence_and_timestamp() {
        let input = MutationEventInput {
            target: EventTarget::Collection,
            operation: EventOperation::Create,
            collection: Slug::new("posts"),
            document_id: DocumentId::new("id1"),
            data: DocumentFields::new(),
            edited_by: None,
            view: crate::core::EventViewMeta::default(),
        };
        let event = stamp_event(input, 42, "node-a");
        assert_eq!(event.sequence, 42);
        assert_eq!(event.publisher, "node-a");
        assert!(!event.timestamp.is_empty());
    }
}
