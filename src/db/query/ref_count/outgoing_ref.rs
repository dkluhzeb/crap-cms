//! [`OutgoingRef`] — a single edge in the doc-to-doc reference graph.

/// An outgoing reference from one document to another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OutgoingRef {
    pub(super) target_collection: String,
    pub(super) target_id: String,
}

/// Parse a reference value string and push an `OutgoingRef` if valid.
///
/// For polymorphic refs, expects `"collection/id"` format.
/// For non-polymorphic, uses `default_collection` as the target.
pub(super) fn push_ref(
    refs: &mut Vec<OutgoingRef>,
    value: &str,
    is_polymorphic: bool,
    default_collection: &str,
) {
    if value.is_empty() {
        return;
    }

    if !is_polymorphic {
        refs.push(OutgoingRef {
            target_collection: default_collection.to_string(),
            target_id: value.to_string(),
        });

        return;
    }

    if let Some((col, id)) = value.split_once('/')
        && !col.is_empty()
        && !id.is_empty()
    {
        refs.push(OutgoingRef {
            target_collection: col.to_string(),
            target_id: id.to_string(),
        });
    }
}
