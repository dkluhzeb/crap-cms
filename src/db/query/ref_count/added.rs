//! [`AddedReferences`] — the references a write adds.

use std::collections::{BTreeMap, BTreeSet};

use super::outgoing_ref::OutgoingRef;

/// The documents a write newly references, per target collection: every
/// target the document holds after the write but did not hold before. A
/// target the document already referenced is not among them — even when the
/// write adds a second reference to it — so a write is judged only on the
/// targets it points at anew.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AddedReferences(BTreeMap<String, Vec<String>>);

impl AddedReferences {
    /// The targets of `new_refs` that `old_refs` does not hold.
    pub(super) fn between(old_refs: &[OutgoingRef], new_refs: &[OutgoingRef]) -> Self {
        let held: BTreeSet<(&str, &str)> = old_refs.iter().map(OutgoingRef::target).collect();
        let fresh: BTreeSet<(&str, &str)> = new_refs
            .iter()
            .map(OutgoingRef::target)
            .filter(|target| !held.contains(target))
            .collect();

        let mut added: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for (collection, id) in fresh {
            added
                .entry(collection.to_string())
                .or_default()
                .push(id.to_string());
        }

        Self(added)
    }

    /// Whether the write adds no reference.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Each target collection with the ids the write newly references in it,
    /// in collection order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.0
            .iter()
            .map(|(collection, ids)| (collection.as_str(), ids.as_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(targets: &[(&str, &str)]) -> Vec<OutgoingRef> {
        targets
            .iter()
            .map(|(collection, id)| OutgoingRef {
                target_collection: (*collection).to_string(),
                target_id: (*id).to_string(),
            })
            .collect()
    }

    fn listed(added: &AddedReferences) -> Vec<(&str, Vec<&str>)> {
        added
            .iter()
            .map(|(collection, ids)| (collection, ids.iter().map(String::as_str).collect()))
            .collect()
    }

    /// Only a target the document did not hold before is added; a dropped
    /// one is not.
    #[test]
    fn only_newly_held_targets_are_added() {
        let old = refs(&[("authors", "a3")]);
        let new = refs(&[("authors", "a2"), ("authors", "a1"), ("tags", "t1")]);

        assert_eq!(
            listed(&AddedReferences::between(&old, &new)),
            vec![("authors", vec!["a1", "a2"]), ("tags", vec!["t1"])]
        );
        assert!(AddedReferences::between(&[], &[]).is_empty());
    }

    /// Regression: a second reference to a target the document already held
    /// (a duplicated array row) counted as a new reference, so a writer who
    /// may no longer read that target was refused as if it pointed at a
    /// missing document.
    #[test]
    fn a_further_reference_to_a_held_target_is_not_added() {
        let old = refs(&[("media", "m1")]);
        let new = refs(&[("media", "m1"), ("media", "m1")]);

        assert!(AddedReferences::between(&old, &new).is_empty());
    }
}
