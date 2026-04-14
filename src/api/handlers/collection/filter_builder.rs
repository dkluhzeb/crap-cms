//! Builder for constructing filter clauses from gRPC request parameters.

use tonic::Status;

use crate::{
    api::handlers::convert::parse_where_json,
    core::FieldDefinition,
    db::{AccessResult, Filter, FilterClause, FilterOp, query::filter::normalize_filter_fields},
    service::read::{validate_access_constraints, validate_user_filters},
};

/// Builder for constructing filter clauses from a gRPC request's `where` JSON,
/// access constraints, and draft filtering. Deduplicates the pattern used across
/// find, count, update_many, and delete_many handlers.
pub(super) struct FilterBuilder<'a> {
    where_json: Option<&'a str>,
    fields: &'a [FieldDefinition],
    access_result: &'a AccessResult,
    has_drafts: bool,
    include_published_only: bool,
    slug: &'a str,
}

impl<'a> FilterBuilder<'a> {
    /// Start with the required params: field definitions and access result.
    pub fn new(fields: &'a [FieldDefinition], access_result: &'a AccessResult) -> Self {
        Self {
            where_json: None,
            fields,
            access_result,
            has_drafts: false,
            include_published_only: false,
            slug: "",
        }
    }

    /// Set the collection slug — used when validating access-hook constraint filters
    /// so the operator-facing error message can name the offending collection.
    pub fn slug(mut self, slug: &'a str) -> Self {
        self.slug = slug;

        self
    }

    /// Set the optional `where` JSON string from the gRPC request.
    pub fn where_json(mut self, json: Option<&'a str>) -> Self {
        self.where_json = json;

        self
    }

    /// Enable draft-aware filtering: if the collection has drafts and we should
    /// only include published documents, a `_status = "published"` filter is added.
    pub fn draft_filter(mut self, has_drafts: bool, include_published_only: bool) -> Self {
        self.has_drafts = has_drafts;
        self.include_published_only = include_published_only;

        self
    }

    /// Build the final filter clause list.
    pub fn build(self) -> Result<Vec<FilterClause>, Status> {
        let mut filters = if let Some(where_json) = self.where_json {
            parse_where_json(where_json)
                .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {}", e)))?
        } else {
            Vec::new()
        };

        // Reject user-supplied filters on system columns (`_*`). The bulk
        // handlers call `query::find` directly rather than going through the
        // service layer, so we validate here before any internal filter
        // injection (draft filter, access constraints) is added to the list.
        validate_user_filters(&filters).map_err(|e| Status::invalid_argument(e.to_string()))?;

        // Normalize dot notation: group dots → __, array/block/rel dots preserved
        normalize_filter_fields(&mut filters, self.fields);

        // Merge access constraint filters. Validate system-column filters
        // against the same allow-list the service layer uses: `_status` is
        // only allowed when the builder is about to inject `_status = published`
        // itself; `_deleted_at` is never allowed on this path (bulk ops do not
        // operate on trash).
        if let AccessResult::Constrained(constraint_filters) = &self.access_result {
            let injecting_status = self.has_drafts && self.include_published_only;
            validate_access_constraints(constraint_filters, false, injecting_status, self.slug)
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
            filters.extend(constraint_filters.iter().cloned());
        }

        // Draft-aware filtering
        if self.has_drafts && self.include_published_only {
            filters.push(FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            }));
        }

        Ok(filters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_builder_empty() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let access = AccessResult::Allowed;
        let filters = FilterBuilder::new(&fields, &access).build().unwrap();
        assert!(filters.is_empty());
    }

    #[test]
    fn filter_builder_with_access_constraints() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let constraint = vec![FilterClause::Single(Filter {
            field: "owner".to_string(),
            op: FilterOp::Equals("user1".to_string()),
        })];
        let access = AccessResult::Constrained(constraint);
        let filters = FilterBuilder::new(&fields, &access).build().unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn filter_builder_with_draft_filter() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let access = AccessResult::Allowed;
        let filters = FilterBuilder::new(&fields, &access)
            .draft_filter(true, true)
            .build()
            .unwrap();
        assert_eq!(filters.len(), 1);
        match &filters[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "_status");
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "published"));
            }
            _ => panic!("Expected Single filter"),
        }
    }

    #[test]
    fn filter_builder_no_draft_when_disabled() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let access = AccessResult::Allowed;
        // has_drafts=false → no draft filter added
        let filters = FilterBuilder::new(&fields, &access)
            .draft_filter(false, true)
            .build()
            .unwrap();
        assert!(filters.is_empty());
    }

    #[test]
    fn filter_builder_no_draft_when_including_drafts() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let access = AccessResult::Allowed;
        // include_published_only=false → no draft filter added
        let filters = FilterBuilder::new(&fields, &access)
            .draft_filter(true, false)
            .build()
            .unwrap();
        assert!(filters.is_empty());
    }

    /// Access-hook filter tables that reference a forbidden system column must
    /// be rejected with an InvalidArgument — bulk handlers call `query::find`
    /// directly, so if the builder silently extended the filter list the
    /// access-hook typo would leak past the validator.
    #[test]
    fn bulk_update_access_hook_returning_underscore_filter_is_rejected() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let constraint = vec![FilterClause::Single(Filter {
            field: "_ref_count".to_string(),
            op: FilterOp::Equals("0".to_string()),
        })];
        let access = AccessResult::Constrained(constraint);

        let err = FilterBuilder::new(&fields, &access)
            .slug("articles")
            .build()
            .unwrap_err();

        let msg = err.message();
        assert!(
            msg.contains("_ref_count"),
            "error should name the forbidden field, got: {msg}"
        );
        assert!(
            msg.contains("articles"),
            "error should name the slug, got: {msg}"
        );
        assert!(
            msg.contains("system column"),
            "error should call out the system column, got: {msg}"
        );
    }

    /// When the builder itself injects `_status = published`, an access-hook
    /// filter on `_status` is acceptable (they combine into a single effective
    /// `_status` filter).
    #[test]
    fn access_hook_status_filter_allowed_when_injecting_status() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let constraint = vec![FilterClause::Single(Filter {
            field: "_status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        })];
        let access = AccessResult::Constrained(constraint);

        FilterBuilder::new(&fields, &access)
            .slug("articles")
            .draft_filter(true, true)
            .build()
            .expect("status constraint allowed when builder also injects _status");
    }

    #[test]
    fn filter_builder_combined_constraints_and_draft() {
        let fields: Vec<FieldDefinition> = Vec::new();
        let constraint = vec![FilterClause::Single(Filter {
            field: "tenant".to_string(),
            op: FilterOp::Equals("acme".to_string()),
        })];
        let access = AccessResult::Constrained(constraint);
        let filters = FilterBuilder::new(&fields, &access)
            .draft_filter(true, true)
            .build()
            .unwrap();
        // 1 access constraint + 1 draft filter
        assert_eq!(filters.len(), 2);
    }
}
