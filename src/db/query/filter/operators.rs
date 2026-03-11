//! Operator SQL generation for individual filter conditions.

use anyhow::{bail, Result};

use super::super::{is_valid_identifier, Filter, FilterOp};

/// Generate a SQL condition applying a [`FilterOp`] to an arbitrary SQL
/// expression, appending bind parameters to `params`.
pub(super) fn build_op_condition(
    expr: &str,
    op: &FilterOp,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> String {
    match op {
        FilterOp::Equals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} = ?", expr)
        }
        FilterOp::NotEquals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} != ?", expr)
        }
        FilterOp::Like(v) => {
            params.push(Box::new(v.clone()));
            format!("{} LIKE ?", expr)
        }
        FilterOp::Contains(v) => {
            let escaped = v.replace('%', "\\%").replace('_', "\\_");
            params.push(Box::new(format!("%{}%", escaped)));
            format!("{} LIKE ? ESCAPE '\\'", expr)
        }
        FilterOp::GreaterThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} > ?", expr)
        }
        FilterOp::LessThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} < ?", expr)
        }
        FilterOp::GreaterThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} >= ?", expr)
        }
        FilterOp::LessThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} <= ?", expr)
        }
        FilterOp::In(vals) => {
            let placeholders: Vec<_> = vals
                .iter()
                .map(|v| {
                    params.push(Box::new(v.clone()));
                    "?".to_string()
                })
                .collect();
            format!("{} IN ({})", expr, placeholders.join(", "))
        }
        FilterOp::NotIn(vals) => {
            let placeholders: Vec<_> = vals
                .iter()
                .map(|v| {
                    params.push(Box::new(v.clone()));
                    "?".to_string()
                })
                .collect();
            format!("{} NOT IN ({})", expr, placeholders.join(", "))
        }
        FilterOp::Exists => {
            format!("{} IS NOT NULL", expr)
        }
        FilterOp::NotExists => {
            format!("{} IS NULL", expr)
        }
    }
}

/// Build a single [`Filter`] into a SQL condition string and append its bind
/// parameters to `params`.
///
/// Defense-in-depth: rejects field names that are not valid SQL identifiers
/// (alphanumeric + underscore), even though higher-level validation should
/// have caught them already.
pub fn build_filter_condition(
    f: &Filter,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
) -> Result<String> {
    if !is_valid_identifier(&f.field) {
        bail!(
            "Invalid field name '{}': must be alphanumeric/underscore",
            f.field
        );
    }
    Ok(build_op_condition(&f.field, &f.op, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::query::{Filter, FilterOp};

    #[test]
    fn filter_condition_equals() {
        let f = Filter {
            field: "status".into(),
            op: FilterOp::Equals("active".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status = ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_not_equals() {
        let f = Filter {
            field: "status".into(),
            op: FilterOp::NotEquals("draft".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status != ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_like() {
        let f = Filter {
            field: "title".into(),
            op: FilterOp::Like("%hello%".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "title LIKE ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_contains() {
        let f = Filter {
            field: "body".into(),
            op: FilterOp::Contains("search term".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "body LIKE ? ESCAPE '\\'");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than() {
        let f = Filter {
            field: "age".into(),
            op: FilterOp::GreaterThan("18".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "age > ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than() {
        let f = Filter {
            field: "price".into(),
            op: FilterOp::LessThan("100".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "price < ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than_or_equal() {
        let f = Filter {
            field: "score".into(),
            op: FilterOp::GreaterThanOrEqual("50".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "score >= ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than_or_equal() {
        let f = Filter {
            field: "rating".into(),
            op: FilterOp::LessThanOrEqual("5".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "rating <= ?");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_in() {
        let f = Filter {
            field: "status".into(),
            op: FilterOp::In(vec!["a".into(), "b".into(), "c".into()]),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "status IN (?, ?, ?)");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn filter_condition_not_in() {
        let f = Filter {
            field: "role".into(),
            op: FilterOp::NotIn(vec!["banned".into(), "suspended".into()]),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "role NOT IN (?, ?)");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn filter_condition_exists() {
        let f = Filter {
            field: "avatar".into(),
            op: FilterOp::Exists,
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "avatar IS NOT NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_not_exists() {
        let f = Filter {
            field: "deleted_at".into(),
            op: FilterOp::NotExists,
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let sql = build_filter_condition(&f, &mut params).unwrap();
        assert_eq!(sql, "deleted_at IS NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_rejects_invalid_identifier() {
        let f = Filter {
            field: "field name".into(),
            op: FilterOp::Equals("v".into()),
        };
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let result = build_filter_condition(&f, &mut params);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid field name"));
    }
}
