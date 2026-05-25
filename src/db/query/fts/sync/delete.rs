//! Per-document FTS index deletion.

use anyhow::{Context as _, Result};

use crate::db::query::fts::search::{fts_table_name, table_exists};
use crate::db::{DbConnection, DbValue};

/// Delete a document from the FTS index.
///
/// No-op if the FTS table doesn't exist.
///
/// # Errors
///
/// Returns a backend error if the DELETE fails.
pub fn fts_delete(conn: &dyn DbConnection, slug: &str, id: &str) -> Result<()> {
    let fts_table = fts_table_name(slug);

    if !table_exists(conn, &fts_table) {
        return Ok(());
    }

    conn.execute(
        &format!(
            "DELETE FROM {} WHERE id = {}",
            fts_table,
            conn.placeholder(1)
        ),
        &[DbValue::Text(id.to_string())],
    )
    .with_context(|| format!("FTS delete in {fts_table}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::db::query::fts::search::fts_search;
    use crate::db::query::fts::sync::sync_fts_table;
    use crate::db::query::fts::sync::test_helpers::*;

    #[test]
    fn delete_removes_from_index() {
        let (_dir, conn) = setup_db();
        insert_post(&conn, "1", "Searchable", "");
        let def = simple_def(vec![text_field("title")]);
        sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        assert_eq!(
            fts_search(&conn, "posts", "Searchable", 10).unwrap().len(),
            1
        );

        fts_delete(&conn, "posts", "1").unwrap();
        assert!(
            fts_search(&conn, "posts", "Searchable", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn delete_noop_no_fts_table() {
        let (_dir, conn) = setup_db();
        fts_delete(&conn, "posts", "1").unwrap();
    }
}
