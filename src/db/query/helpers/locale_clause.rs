//! The `_locale NOT IN (…)` clause shared by the stale-locale row count and
//! delete behind `db cleanup`.

use crate::db::{DbConnection, DbValue, query::helpers::placeholder_list};

/// `_locale NOT IN (…)` plus the bound locale values, for a table whose rows
/// belong to `locales`. One builder for the count and the delete so they can
/// never disagree about which rows are stale.
pub(in crate::db::query) fn outside_locales_clause(
    conn: &dyn DbConnection,
    locales: &[String],
) -> (String, Vec<DbValue>) {
    let placeholders = placeholder_list(conn, locales.len());

    let params = locales
        .iter()
        .map(|l| DbValue::Text(l.clone()))
        .collect::<Vec<_>>();

    (format!("_locale NOT IN ({placeholders})"), params)
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    /// One placeholder and one bound value per configured locale, in order.
    #[test]
    fn binds_one_value_per_locale() {
        let conn = InMemoryConn::open();
        let (clause, params) = outside_locales_clause(&conn, &["en".to_string(), "de".to_string()]);

        assert_eq!(clause, "_locale NOT IN (?1, ?2)");
        assert_eq!(
            params,
            vec![DbValue::Text("en".into()), DbValue::Text("de".into())]
        );
    }
}
