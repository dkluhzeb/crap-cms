//! Default draft-visibility rule, shared by the `find` and `search` read paths.
//!
//! Both paths must hide unpublished drafts identically on a default read; a
//! divergence between two inline copies would be a security bug (drafts
//! leaking from one surface but not the other). The single rule lives here.

use crate::core::CollectionDefinition;
use crate::db::{Filter, FilterClause, FilterOp};

/// The `_status = "published"` filter that hides unpublished drafts, or `None`
/// when drafts should be visible.
///
/// Returns `Some(..)` exactly when the collection has drafts **and** the caller
/// did not explicitly opt into them (`include_drafts == false`).
pub(crate) fn draft_visibility_filter(
    def: &CollectionDefinition,
    include_drafts: bool,
) -> Option<FilterClause> {
    (!include_drafts && def.has_drafts()).then(|| {
        FilterClause::Single(Filter {
            field: "_status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        })
    })
}

#[cfg(test)]
mod tests {
    use crate::core::collection::VersionsConfig;

    use super::*;

    fn drafts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.versions = Some(VersionsConfig::new(true, 5));
        def
    }

    fn is_published_status(c: &FilterClause) -> bool {
        matches!(c, FilterClause::Single(f)
            if f.field == "_status" && matches!(&f.op, FilterOp::Equals(v) if v == "published"))
    }

    #[test]
    fn hides_drafts_by_default_on_a_drafts_collection() {
        let f = draft_visibility_filter(&drafts_def(), false).expect("filter present");
        assert!(is_published_status(&f));
    }

    #[test]
    fn none_when_drafts_explicitly_included() {
        assert!(draft_visibility_filter(&drafts_def(), true).is_none());
    }

    #[test]
    fn none_when_collection_has_no_drafts() {
        // include_drafts=false but no drafts configured → nothing to hide.
        assert!(draft_visibility_filter(&CollectionDefinition::new("posts"), false).is_none());
    }
}
