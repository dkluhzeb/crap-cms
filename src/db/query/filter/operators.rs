//! Operator SQL generation for individual filter conditions.

use anyhow::{Result, bail};

use crate::db::{DbConnection, DbValue, Filter, FilterOp, query::is_valid_identifier};

/// Generate a SQL condition applying a [`FilterOp`] to an arbitrary SQL
/// expression, appending bind parameters to `params`.
pub(super) fn build_op_condition(
    conn: &dyn DbConnection,
    expr: &str,
    op: &FilterOp,
    params: &mut Vec<DbValue>,
) -> String {
    match op {
        FilterOp::Equals(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} = {}", expr, ph)
        }
        FilterOp::NotEquals(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} != {}", expr, ph)
        }
        FilterOp::Like(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} {} {}", expr, conn.like_operator(), ph)
        }
        FilterOp::Contains(v) => {
            let escaped = v.replace('%', "\\%").replace('_', "\\_");
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(format!("%{}%", escaped)));
            format!("{} {} {} ESCAPE '\\'", expr, conn.like_operator(), ph)
        }
        FilterOp::GreaterThan(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} > {}", expr, ph)
        }
        FilterOp::LessThan(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} < {}", expr, ph)
        }
        FilterOp::GreaterThanOrEqual(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} >= {}", expr, ph)
        }
        FilterOp::LessThanOrEqual(v) => {
            let ph = conn.placeholder(params.len() + 1);
            params.push(DbValue::Text(v.clone()));
            format!("{} <= {}", expr, ph)
        }
        FilterOp::In(vals) => {
            if vals.is_empty() {
                return "0 = 1".to_string();
            }

            let placeholders: Vec<_> = vals
                .iter()
                .map(|v| {
                    let ph = conn.placeholder(params.len() + 1);
                    params.push(DbValue::Text(v.clone()));
                    ph
                })
                .collect();
            format!("{} IN ({})", expr, placeholders.join(", "))
        }
        FilterOp::NotIn(vals) => {
            if vals.is_empty() {
                return "1 = 1".to_string();
            }

            let placeholders: Vec<_> = vals
                .iter()
                .map(|v| {
                    let ph = conn.placeholder(params.len() + 1);
                    params.push(DbValue::Text(v.clone()));
                    ph
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
    conn: &dyn DbConnection,
    f: &Filter,
    params: &mut Vec<DbValue>,
) -> Result<String> {
    if !is_valid_identifier(&f.field) {
        bail!(
            "Invalid field name '{}': must be alphanumeric/underscore",
            f.field
        );
    }
    Ok(build_op_condition(conn, &f.field, &f.op, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite::InMemoryConn;
    use crate::db::{
        DbValue,
        query::{Filter, FilterOp},
    };

    fn conn() -> InMemoryConn {
        InMemoryConn::open()
    }

    #[test]
    fn filter_condition_equals() {
        let c = conn();
        let f = Filter {
            field: "status".into(),
            op: FilterOp::Equals("active".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "status = ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_not_equals() {
        let c = conn();
        let f = Filter {
            field: "status".into(),
            op: FilterOp::NotEquals("draft".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "status != ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_like() {
        let c = conn();
        let f = Filter {
            field: "title".into(),
            op: FilterOp::Like("%hello%".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "title LIKE ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_contains() {
        let c = conn();
        let f = Filter {
            field: "body".into(),
            op: FilterOp::Contains("search term".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "body LIKE ?1 ESCAPE '\\'");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than() {
        let c = conn();
        let f = Filter {
            field: "age".into(),
            op: FilterOp::GreaterThan("18".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "age > ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than() {
        let c = conn();
        let f = Filter {
            field: "price".into(),
            op: FilterOp::LessThan("100".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "price < ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_greater_than_or_equal() {
        let c = conn();
        let f = Filter {
            field: "score".into(),
            op: FilterOp::GreaterThanOrEqual("50".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "score >= ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_less_than_or_equal() {
        let c = conn();
        let f = Filter {
            field: "rating".into(),
            op: FilterOp::LessThanOrEqual("5".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "rating <= ?1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn filter_condition_in() {
        let c = conn();
        let f = Filter {
            field: "status".into(),
            op: FilterOp::In(vec!["a".into(), "b".into(), "c".into()]),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "status IN (?1, ?2, ?3)");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn filter_condition_not_in() {
        let c = conn();
        let f = Filter {
            field: "role".into(),
            op: FilterOp::NotIn(vec!["banned".into(), "suspended".into()]),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "role NOT IN (?1, ?2)");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn filter_condition_exists() {
        let c = conn();
        let f = Filter {
            field: "avatar".into(),
            op: FilterOp::Exists,
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "avatar IS NOT NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_not_exists() {
        let c = conn();
        let f = Filter {
            field: "deleted_at".into(),
            op: FilterOp::NotExists,
        };
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_filter_condition(&c, &f, &mut params).unwrap();
        assert_eq!(sql, "deleted_at IS NULL");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_in_empty_is_false() {
        let c = conn();
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_op_condition(&c, "status", &FilterOp::In(vec![]), &mut params);
        assert_eq!(sql, "0 = 1");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_not_in_empty_is_true() {
        let c = conn();
        let mut params: Vec<DbValue> = Vec::new();
        let sql = build_op_condition(&c, "status", &FilterOp::NotIn(vec![]), &mut params);
        assert_eq!(sql, "1 = 1");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn filter_condition_rejects_invalid_identifier() {
        let c = conn();
        let f = Filter {
            field: "field name".into(),
            op: FilterOp::Equals("v".into()),
        };
        let mut params: Vec<DbValue> = Vec::new();
        let result = build_filter_condition(&c, &f, &mut params);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid field name")
        );
    }
}
