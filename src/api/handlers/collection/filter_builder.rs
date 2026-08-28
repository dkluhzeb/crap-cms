//! Decode a gRPC request's `where` JSON into the canonical filter grammar.

use tonic::Status;

use crate::{api::handlers::proto::parse_where_json, db::FilterClause};

/// Parse the optional `where` JSON string into filter clauses. Pure decode —
/// system-column rejection and dot-notation normalization live in the shared
/// operation bodies (`Find`/`Count`/`UpdateMany`/`DeleteMany`), identical on
/// every surface, so nothing beyond wire parsing belongs here.
///
/// # Errors
///
/// Returns `InvalidArgument` when the JSON does not parse into the filter
/// grammar.
pub(super) fn decode_where_json(where_json: Option<&str>) -> Result<Vec<FilterClause>, Status> {
    let Some(where_json) = where_json else {
        return Ok(Vec::new());
    };

    parse_where_json(where_json)
        .map_err(|e| Status::invalid_argument(format!("Invalid where clause: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_where_json_empty() {
        let filters = decode_where_json(None).unwrap();
        assert!(filters.is_empty());
    }

    #[test]
    fn decode_where_json_parses() {
        let filters = decode_where_json(Some(r#"{"title":{"equals":"x"}}"#)).unwrap();
        assert_eq!(filters.len(), 1);
    }

    #[test]
    fn decode_where_json_rejects_invalid_json() {
        let err = decode_where_json(Some("not json")).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
