//! Monotonic sequence generator + event-stamping helper shared between transports.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::types::{MutationEvent, MutationEventInput};

/// Monotonic sequence generator shared between transports. Starts at 1.
#[derive(Clone)]
pub(crate) struct SequenceGen {
    counter: Arc<AtomicU64>,
}

impl SequenceGen {
    pub(crate) fn new() -> Self {
        Self {
            counter: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::AcqRel)
    }
}

/// Build a [`MutationEvent`] from an input plus a fresh sequence number and
/// timestamp.
pub(crate) fn stamp_event(input: MutationEventInput, sequence: u64) -> MutationEvent {
    let MutationEventInput {
        target,
        operation,
        collection,
        document_id,
        data,
        edited_by,
    } = input;

    MutationEvent {
        sequence,
        timestamp: chrono::Utc::now().to_rfc3339(),
        target,
        operation,
        collection,
        document_id,
        data,
        edited_by,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::core::{
        DocumentId, Slug,
        event::types::{EventOperation, EventTarget},
    };

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
            data: HashMap::new(),
            edited_by: None,
        };
        let event = stamp_event(input, 42);
        assert_eq!(event.sequence, 42);
        assert!(!event.timestamp.is_empty());
    }
}
