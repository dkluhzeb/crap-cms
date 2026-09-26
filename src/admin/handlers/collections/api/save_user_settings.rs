use std::collections::HashMap;

use anyhow::Error;
use axum::{
    Extension, Form,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};

use crate::{
    admin::{
        AdminState,
        handlers::shared::{is_column_eligible, is_meta_column},
    },
    core::{CollectionDefinition, auth::AuthUser, spawn_request_blocking},
    db::DbPool,
    service::user_settings,
};

/// Parse and validate column keys from the form against the collection definition.
///
/// A key survives only if the collection's table actually has the column:
/// `is_meta_column` is the same predicate the sort gate and the list-view
/// header use, so a stored preference can never name a column the list then
/// fails to render (`_status` on a collection without drafts).
fn parse_valid_columns(form: &HashMap<String, String>, def: &CollectionDefinition) -> Vec<String> {
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
            is_meta_column(k, def)
                || def
                    .fields
                    .iter()
                    .any(|f| f.name == *k && is_column_eligible(&f.field_type))
        })
        .collect()
}

/// Store the column preferences for one collection, leaving the user's other
/// settings as they are (see [`user_settings::update_user_settings`]).
fn save_column_preferences(
    pool: &DbPool,
    user_id: &str,
    collection_slug: &str,
    columns: &[String],
) -> Result<(), Error> {
    user_settings::update_user_settings(pool, user_id, |settings| {
        settings.set_columns(collection_slug, columns);
    })?;

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

    let result = spawn_request_blocking(move || {
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
    use crate::core::VersionsConfig;
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
    fn always_allows_the_timestamp_columns_even_without_matching_fields() {
        let def = def_with(vec![]);
        let cols = parse_valid_columns(&form("created_at,updated_at"), &def);
        assert_eq!(cols, vec!["created_at", "updated_at"]);
    }

    /// Regression: `_status` is a column only on a collection that keeps
    /// drafts. Storing it as a preference elsewhere put a column the table
    /// never had into the list view.
    #[test]
    fn status_is_only_storable_on_a_collection_with_drafts() {
        let def = def_with(vec![]);
        assert!(parse_valid_columns(&form("_status"), &def).is_empty());

        let mut with_drafts = def_with(vec![]);
        with_drafts.versions = Some(VersionsConfig::new(true, 10));
        assert_eq!(
            parse_valid_columns(&form("_status"), &with_drafts),
            vec!["_status".to_string()]
        );
    }

    #[test]
    fn missing_columns_key_yields_empty() {
        let def = def_with(vec![("title", FieldType::Text)]);
        assert!(parse_valid_columns(&HashMap::new(), &def).is_empty());
    }
}
