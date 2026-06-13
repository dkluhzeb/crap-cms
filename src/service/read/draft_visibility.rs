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

/// Whether a read's effective query would expose draft (unpublished) rows, and
/// therefore must be gated at edit level (`access.draft ?? access.update`)
/// rather than by `access.read`.
///
/// Mirrors `build_effective_query`'s status logic so the access gate and the
/// query agree: an explicit `status_filter` exposes drafts when it names any
/// non-`published` status; otherwise the default-draft rule exposes them when
/// the caller opted in via `include_drafts`. Non-draft collections never
/// expose drafts.
pub(crate) fn read_exposes_drafts(
    def: &CollectionDefinition,
    status_filter: Option<&[String]>,
    include_drafts: bool,
) -> bool {
    if !def.has_drafts() {
        return false;
    }

    match status_filter {
        Some(values) if !values.is_empty() => values.iter().any(|s| s != "published"),
        _ => include_drafts,
    }
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

    #[test]
    fn exposes_drafts_via_include_drafts_opt_in() {
        let def = drafts_def();
        assert!(read_exposes_drafts(&def, None, true));
        assert!(!read_exposes_drafts(&def, None, false));
    }

    #[test]
    fn exposes_drafts_when_status_filter_names_non_published() {
        let def = drafts_def();
        // A status filter naming a draft status exposes drafts even with
        // include_drafts=false — the gate must catch `?where[_status]=draft`.
        let draft_only = [String::from("draft")];
        assert!(read_exposes_drafts(&def, Some(&draft_only), false));

        let mixed = [String::from("draft"), String::from("published")];
        assert!(read_exposes_drafts(&def, Some(&mixed), false));
    }

    #[test]
    fn published_only_status_filter_does_not_expose_drafts() {
        let def = drafts_def();
        let published_only = [String::from("published")];
        // Restricting to published must NOT trip the draft gate, even with
        // include_drafts=true (the filter wins and hides drafts).
        assert!(!read_exposes_drafts(&def, Some(&published_only), true));
    }

    #[test]
    fn non_draft_collection_never_exposes_drafts() {
        let def = CollectionDefinition::new("posts");
        let draft_only = [String::from("draft")];
        assert!(!read_exposes_drafts(&def, Some(&draft_only), true));
        assert!(!read_exposes_drafts(&def, None, true));
    }
}
