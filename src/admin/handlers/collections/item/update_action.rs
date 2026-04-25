use axum::{
    Extension,
    extract::{Path, Request, State},
    response::Response,
};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::{
            collections::shared::{delete_action_impl, do_update},
            forms::parse_form,
            shared::{paths, redirect_response},
        },
    },
    core::auth::AuthUser,
};

/// POST handler for update/delete (HTML forms use _method override).
pub async fn update_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
    request: Request,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    let (mut form_data, file) = match parse_form(request, &state, &def).await {
        Ok(result) => result,
        Err(e) => {
            error!("{}", e);
            return redirect_response(&paths::collection_item(&slug, &id));
        }
    };

    let method = form_data.remove("_method").unwrap_or_default();

    if method.eq_ignore_ascii_case("DELETE") {
        return delete_action_impl(&state, &slug, &id, &auth_user, false, false).await;
    }

    do_update(&state, &slug, &id, form_data, file, &auth_user).await
}
