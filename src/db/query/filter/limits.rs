//! Size limits on a caller-supplied `where` clause.
//!
//! A `where` is cheap to send and expensive to run: every condition becomes a
//! predicate (or a correlated `EXISTS` subquery for a path into an array or
//! blocks field) evaluated against every candidate row, once for the page and
//! again for its count. These checks bound that cost before any SQL is built.

use anyhow::Result;

use crate::{
    config::QueryConfig,
    db::{Filter, FilterClause, FilterOp, query::filter::invalid_query},
};

/// The deepest `and`/`or` nesting a user filter may have. The `where` grammar
/// produces at most an `or` of `and` groups (depth 3); the bound keeps any
/// future grammar from building an unbounded tree.
const MAX_FILTER_DEPTH: usize = 8;

/// The key a width rejection names — the whole clause, not one field.
const WHERE_KEY: &str = "where";

/// Running totals of one `where`'s size.
#[derive(Default)]
struct Tally {
    terms: usize,
    values: usize,
}

/// Reject a user `where` wider or deeper than `limits` allow: more conditions
/// than `max_filter_terms` (counted across every `or` group), more `in` /
/// `not_in` elements than `max_filter_values` (summed over every list), or
/// nesting deeper than the grammar produces.
///
/// # Errors
///
/// Returns a typed invalid-query error (`ValidationError` naming `where`), so
/// every surface answers invalid-argument.
pub fn check_filter_limits(filters: &[FilterClause], limits: &QueryConfig) -> Result<()> {
    let mut tally = Tally::default();

    for clause in filters {
        tally_clause(clause, 1, &mut tally, limits)?;
    }

    Ok(())
}

/// Add `clause` (at nesting `depth`) to `tally`, failing as soon as a limit is
/// crossed so a huge clause is never walked in full.
fn tally_clause(
    clause: &FilterClause,
    depth: usize,
    tally: &mut Tally,
    limits: &QueryConfig,
) -> Result<()> {
    if depth > MAX_FILTER_DEPTH {
        return Err(invalid_query(
            WHERE_KEY,
            format!("filter nesting exceeds the maximum depth of {MAX_FILTER_DEPTH}"),
        ));
    }

    match clause {
        FilterClause::Single(filter) => tally_filter(filter, tally, limits),
        FilterClause::And(subs) | FilterClause::Or(subs) => subs
            .iter()
            .try_for_each(|sub| tally_clause(sub, depth + 1, tally, limits)),
    }
}

/// Count one condition and its list elements.
fn tally_filter(filter: &Filter, tally: &mut Tally, limits: &QueryConfig) -> Result<()> {
    tally.terms += 1;

    let max_terms = limits.max_filter_terms.get();

    if tally.terms > max_terms {
        return Err(invalid_query(
            WHERE_KEY,
            format!("too many filter conditions (at most {max_terms}, `[query] max_filter_terms`)"),
        ));
    }

    let (FilterOp::In(values) | FilterOp::NotIn(values)) = &filter.op else {
        return Ok(());
    };

    tally.values += values.len();

    let max_values = limits.max_filter_values.get();

    if tally.values > max_values {
        return Err(invalid_query(
            WHERE_KEY,
            format!(
                "too many `in` / `not_in` values (at most {max_values} in total, \
                 `[query] max_filter_values`)"
            ),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;
    use crate::core::ValidationError;

    fn limits(terms: usize, values: usize) -> QueryConfig {
        QueryConfig {
            max_filter_terms: NonZeroUsize::new(terms).unwrap(),
            max_filter_values: NonZeroUsize::new(values).unwrap(),
            ..QueryConfig::default()
        }
    }

    fn eq(field: &str) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::Equals("x".to_string()),
        })
    }

    fn one_of(field: &str, n: usize) -> FilterClause {
        FilterClause::Single(Filter {
            field: field.to_string(),
            op: FilterOp::In((0..n).map(|i| i.to_string()).collect()),
        })
    }

    fn message(err: &anyhow::Error) -> String {
        let ve = err.downcast_ref::<ValidationError>().expect("typed error");

        assert_eq!(ve.errors[0].field, "where");

        ve.errors[0].message.clone()
    }

    #[test]
    fn a_where_within_the_limits_passes() {
        let filters = vec![eq("a"), FilterClause::Or(vec![eq("b"), one_of("c", 3)])];

        check_filter_limits(&filters, &limits(3, 3)).expect("within limits");
    }

    /// Regression: a `where` of thousands of `or` alternatives was accepted
    /// anonymously and evaluated against every row. Terms are counted across
    /// every group, not per group.
    #[test]
    fn terms_are_counted_across_or_groups() {
        let wide = FilterClause::Or((0..4).map(|i| eq(&format!("f{i}"))).collect());

        let err = check_filter_limits(&[wide], &limits(3, 100)).unwrap_err();

        assert!(message(&err).contains("max_filter_terms"));
    }

    #[test]
    fn in_values_are_summed_over_every_list() {
        let filters = vec![one_of("a", 2), one_of("b", 2)];

        let err = check_filter_limits(&filters, &limits(10, 3)).unwrap_err();

        assert!(message(&err).contains("max_filter_values"));
    }

    #[test]
    fn not_in_values_count_too() {
        let filter = FilterClause::Single(Filter {
            field: "a".to_string(),
            op: FilterOp::NotIn(vec!["1".into(), "2".into()]),
        });

        assert!(check_filter_limits(&[filter], &limits(10, 1)).is_err());
    }

    #[test]
    fn nesting_deeper_than_the_grammar_is_refused() {
        let mut clause = eq("a");

        for _ in 0..MAX_FILTER_DEPTH {
            clause = FilterClause::And(vec![clause]);
        }

        let err = check_filter_limits(&[clause], &limits(100, 100)).unwrap_err();

        assert!(message(&err).contains("depth"));
    }

    #[test]
    fn the_default_limits_accept_an_ordinary_query() {
        let filters = vec![eq("a"), one_of("b", 50)];

        check_filter_limits(&filters, &QueryConfig::default()).expect("ordinary query");
    }
}
