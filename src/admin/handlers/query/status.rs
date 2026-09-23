//! The typed `_status` filter of a list URL.
//!
//! `_status` is a system column (`_*` prefix) and is therefore rejected by
//! `parse_where_params` and `validate_user_filters`. The admin filter drawer
//! routes it through [`extract_status_filter`] instead so it can ride a
//! service-layer typed param (`FindDocumentsInput::status_filter`) and bypass
//! user-filter validation safely. The service maps the requested statuses to
//! content views (`service::requested_views`), which `ViewScope` resolves and
//! gates per view.
//!
//! The rows combine exactly as the generic filter rows do: every top-level row
//! and every row of one OR bucket must match, so `status is draft AND status is
//! published` admits no document; the buckets of an OR group are a union.

use std::collections::BTreeMap;

use super::{
    filter::{parse_or_key, parse_top_key},
    url::url_decode,
};

/// One `_status` `equals` row: its OR position (`None` at the top level) and
/// the status it names.
struct StatusRow {
    or_position: Option<(usize, usize)>,
    value: String,
}

/// The `_status` rows of a list URL, read as the typed status filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusFilter {
    named: Vec<String>,
    admitted: Vec<String>,
}

impl StatusFilter {
    pub(crate) fn new(named: Vec<String>, admitted: Vec<String>) -> Self {
        Self { named, admitted }
    }

    /// Every distinct status the rows name, in URL order.
    pub(crate) fn named(&self) -> &[String] {
        &self.named
    }

    /// The statuses a document may hold and still match every row, in URL
    /// order.
    pub(crate) fn admitted(&self) -> &[String] {
        &self.admitted
    }

    /// Whether the rows contradict each other — `draft AND published` — so no
    /// document matches.
    pub(crate) fn matches_nothing(&self) -> bool {
        self.admitted.is_empty()
    }

    /// The statuses the read requests: the admitted ones — or, when the rows
    /// admit none, every named one, so the read still resolves the views the
    /// viewer may see while a match-nothing filter returns no row.
    pub(crate) fn requested(&self) -> Vec<String> {
        if self.matches_nothing() {
            return self.named.clone();
        }

        self.admitted.clone()
    }
}

/// Read one query entry as a `_status` `equals` row, when it is one with a
/// value. Accepts raw (`where[_status][equals]=draft`), URL-encoded
/// (`where%5B_status%5D%5Bequals%5D=draft`) and OR-bucket
/// (`where[or][G][N][_status][equals]=…`) forms.
fn status_row(part: &str) -> Option<StatusRow> {
    let (key, value) = part.split_once('=')?;
    let key = url_decode(key);

    let (or_position, field, op) = if let Some((field, op)) = parse_top_key(&key) {
        (None, field, op)
    } else {
        let (group, bucket, field, op) = parse_or_key(&key)?;
        (Some((group, bucket)), field, op)
    };

    if field != "_status" || op != "equals" {
        return None;
    }

    let value = url_decode(value);

    (!value.is_empty()).then_some(StatusRow { or_position, value })
}

/// Whether a document of `status` matches every row: each top-level row names
/// it, and each OR group has a bucket whose rows all name it.
fn admits(rows: &[StatusRow], status: &str) -> bool {
    let mut groups: BTreeMap<usize, BTreeMap<usize, bool>> = BTreeMap::new();

    for row in rows {
        let names_it = row.value == status;

        let Some((group, bucket)) = row.or_position else {
            if !names_it {
                return false;
            }

            continue;
        };

        let bucket_ok = groups
            .entry(group)
            .or_default()
            .entry(bucket)
            .or_insert(true);
        *bucket_ok &= names_it;
    }

    groups
        .values()
        .all(|buckets| buckets.values().any(|bucket_ok| *bucket_ok))
}

/// Extract the typed `_status` filter from a raw URL query string. Returns
/// `None` when the query holds no `_status` `equals` row with a value.
pub(crate) fn extract_status_filter(raw_query: &str) -> Option<StatusFilter> {
    let rows: Vec<StatusRow> = raw_query.split('&').filter_map(status_row).collect();

    if rows.is_empty() {
        return None;
    }

    let mut named: Vec<String> = Vec::new();

    for row in &rows {
        if !named.contains(&row.value) {
            named.push(row.value.clone());
        }
    }

    let admitted = named
        .iter()
        .filter(|status| admits(&rows, status))
        .cloned()
        .collect();

    Some(StatusFilter::new(named, admitted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    fn filter(named: &[&str], admitted: &[&str]) -> StatusFilter {
        StatusFilter::new(strings(named), strings(admitted))
    }

    #[test]
    fn extract_status_filter_raw() {
        assert_eq!(
            extract_status_filter("page=1&where[_status][equals]=draft"),
            Some(filter(&["draft"], &["draft"]))
        );
    }

    #[test]
    fn extract_status_filter_url_encoded() {
        assert_eq!(
            extract_status_filter("page=1&where%5B_status%5D%5Bequals%5D=draft"),
            Some(filter(&["draft"], &["draft"]))
        );
    }

    #[test]
    fn extract_status_filter_absent() {
        assert_eq!(extract_status_filter("page=1&sort=created_at"), None);
        assert_eq!(extract_status_filter(""), None);
    }

    #[test]
    fn extract_status_filter_only_equals_op() {
        assert_eq!(
            extract_status_filter("where[_status][not_equals]=draft"),
            None
        );
    }

    #[test]
    fn extract_status_filter_empty_value_is_none() {
        assert_eq!(extract_status_filter("where[_status][equals]="), None);
    }

    #[test]
    fn extract_status_filter_ignores_user_status_field() {
        assert_eq!(extract_status_filter("where[status][equals]=draft"), None);
    }

    /// The buckets of an OR group are a union.
    #[test]
    fn or_buckets_are_a_union() {
        assert_eq!(
            extract_status_filter(
                "where[or][0][0][_status][equals]=draft&where[or][0][1][_status][equals]=published",
            ),
            Some(filter(&["draft", "published"], &["draft", "published"]))
        );
    }

    /// Regression: top-level `_status` rows were unioned although every other
    /// top-level row is combined with AND — `status is draft AND status is published`
    /// listed both drafts and published documents instead of none.
    #[test]
    fn top_level_rows_are_an_intersection() {
        let contradiction =
            extract_status_filter("where[_status][equals]=draft&where[_status][equals]=published")
                .unwrap();

        assert!(contradiction.matches_nothing());
        assert_eq!(contradiction.named(), strings(&["draft", "published"]));
        assert_eq!(contradiction.requested(), strings(&["draft", "published"]));

        assert_eq!(
            extract_status_filter("where[_status][equals]=draft&where[_status][equals]=draft"),
            Some(filter(&["draft"], &["draft"]))
        );
    }

    /// The rows of one OR bucket are combined with AND too.
    #[test]
    fn rows_of_one_bucket_are_an_intersection() {
        let got = extract_status_filter(
            "where[or][0][0][_status][equals]=draft&where[or][0][0][_status][equals]=published\
             &where[or][0][1][_status][equals]=published",
        );

        assert_eq!(got, Some(filter(&["draft", "published"], &["published"])));
    }

    /// A top-level row narrows an OR group's union to what both admit.
    #[test]
    fn a_top_level_row_narrows_an_or_group() {
        let narrowed = extract_status_filter(
            "where[_status][equals]=draft\
             &where[or][0][0][_status][equals]=draft&where[or][0][1][_status][equals]=published",
        );
        assert_eq!(narrowed, Some(filter(&["draft", "published"], &["draft"])));

        let disjoint = extract_status_filter(
            "where[_status][equals]=draft&where[or][0][0][_status][equals]=published",
        )
        .unwrap();
        assert!(disjoint.matches_nothing());
        assert!(disjoint.admitted().is_empty());
    }
}
