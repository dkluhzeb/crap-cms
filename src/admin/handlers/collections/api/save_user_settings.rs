use std::collections::HashMap;

use anyhow::{Context as _, Error};
use axum::{
    Extension, Form,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, from_str, json, to_string};
use tokio::task;

use crate::{
    admin::{AdminState, handlers::shared::is_column_eligible},
    core::auth::AuthUser,
    db::DbPool,
    service::user_settings,
};

/// Parse and validate column keys from the form against the collection definition.
fn parse_valid_columns(
    form: &HashMap<String, String>,
    def: &crate::core::CollectionDefinition,
) -> Vec<String> {
    let columns: Vec<String> = form
        .get("columns")
        .map(|c| {
            c.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    columns
        .into_iter()
        .filter(|k| {
            k == "created_at"
                || k == "updated_at"
                || k == "_status"
                || def
                    .fields
                    .iter()
                    .any(|f| f.name == *k && is_column_eligible(&f.field_type))
        })
        .collect()
}

/// Load the user's settings JSON, merge column preferences for one collection, and save.
fn save_column_preferences(
    pool: &DbPool,
    user_id: &str,
    collection_slug: &str,
    columns: &[String],
) -> Result<(), Error> {
    // IMMEDIATE tx so the read-modify-write of the whole-blob settings
    // JSON can't lose a concurrent update from a sibling handler
    // (ledger class L5: two tabs / auto-save + locale switch each read
    // the blob, merge their own key, and the second write clobbered the
    // first). The IMMEDIATE lock serializes the read against other
    // writers.
    let mut conn = pool.get().context("Failed to get DB connection")?;
    let tx = conn
        .transaction_immediate()
        .context("Failed to start settings transaction")?;

    let existing = user_settings::get_user_settings(&tx, user_id)?;

    let mut settings: Value = existing
        .as_deref()
        .and_then(|s| from_str(s).ok())
        .unwrap_or_else(|| json!({}));

    settings[collection_slug] = json!({ "columns": columns });

    let json_str = to_string(&settings)?;

    user_settings::set_user_settings(&tx, user_id, &json_str)?;
    tx.commit().context("Failed to commit settings")?;

    Ok(())
}

/// POST /admin/api/user-settings/{slug} — save user column preferences
pub async fn save_user_settings(
    State(state): State<AdminState>,
    Path(collection_slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let Some(Extension(auth_user)) = auth_user else {
        return StatusCode::UNAUTHORIZED;
    };

    let Some(def) = state
        .infra
        .registry
        .get_collection(&collection_slug)
        .cloned()
    else {
        return StatusCode::NOT_FOUND;
    };

    let valid_columns = parse_valid_columns(&form, &def);
    let pool = state.infra.pool.clone();
    let user_id = auth_user.claims.sub.clone();

    let result = task::spawn_blocking(move || {
        save_column_preferences(&pool, &user_id, &collection_slug, &valid_columns)
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::core::CollectionDefinition;
    use crate::core::field::{FieldDefinition, FieldType};

    use super::*;

    fn def_with(fields: Vec<(&str, FieldType)>) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = fields
            .into_iter()
            .map(|(n, t)| FieldDefinition::builder(n, t).build())
            .collect();
        def
    }

    fn form(columns: &str) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("columns".to_string(), columns.to_string());
        m
    }

    #[test]
    fn keeps_only_schema_eligible_and_whitelisted_columns() {
        let def = def_with(vec![
            ("title", FieldType::Text),
            ("body", FieldType::Blocks),
        ]);
        // title (Text, eligible) + created_at (always allowed) kept;
        // body (Blocks, not eligible) and unknown column dropped.
        let cols = parse_valid_columns(&form(" title , created_at , body , evil "), &def);
        assert_eq!(cols, vec!["title".to_string(), "created_at".to_string()]);
    }

    #[test]
    fn always_allows_meta_columns_even_without_matching_fields() {
        let def = def_with(vec![]);
        let cols = parse_valid_columns(&form("created_at,updated_at,_status"), &def);
        assert_eq!(cols, vec!["created_at", "updated_at", "_status"]);
    }

    #[test]
    fn missing_columns_key_yields_empty() {
        let def = def_with(vec![("title", FieldType::Text)]);
        assert!(parse_valid_columns(&HashMap::new(), &def).is_empty());
    }
}
