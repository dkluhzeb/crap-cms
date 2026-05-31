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

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(refs: &[OutgoingRef]) -> Vec<(&str, &str)> {
        refs.iter()
            .map(|r| (r.target_collection.as_str(), r.target_id.as_str()))
            .collect()
    }

    #[test]
    fn empty_value_pushes_nothing() {
        let mut refs = Vec::new();
        push_ref(&mut refs, "", false, "tags");
        push_ref(&mut refs, "", true, "");
        assert!(refs.is_empty());
    }

    #[test]
    fn non_polymorphic_uses_the_default_collection() {
        let mut refs = Vec::new();
        push_ref(&mut refs, "t1", false, "tags");
        assert_eq!(pairs(&refs), vec![("tags", "t1")]);
    }

    #[test]
    fn polymorphic_parses_collection_slash_id() {
        let mut refs = Vec::new();
        push_ref(&mut refs, "posts/p1", true, "");
        assert_eq!(pairs(&refs), vec![("posts", "p1")]);
    }

    #[test]
    fn malformed_polymorphic_values_are_dropped() {
        let mut refs = Vec::new();
        push_ref(&mut refs, "posts", true, ""); // no separator
        push_ref(&mut refs, "/p1", true, ""); // empty collection
        push_ref(&mut refs, "posts/", true, ""); // empty id
        assert!(
            refs.is_empty(),
            "malformed polymorphic refs must not be counted: {:?}",
            pairs(&refs)
        );
    }
}
