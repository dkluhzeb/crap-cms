use axum::{
    Extension,
    extract::{Form, FromRequest, Path, State},
    response::IntoResponse,
};
use std::collections::HashMap;

use crate::admin::AdminState;
use crate::admin::handlers::collections::forms::parse_multipart_form;
use crate::admin::handlers::collections::shared::do_update;
use crate::admin::handlers::shared::redirect_response;
use crate::core::auth::AuthUser;

/// POST handler for update/delete (HTML forms use _method override).
pub async fn update_action(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    // Parse form data — multipart for upload collections, regular form for others
    let (mut form_data, file) = if def.is_upload_collection() {
        match parse_multipart_form(request, &state).await {
            Ok(result) => result,
            Err(e) => {
                tracing::error!("Multipart parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
            }
        }
    } else {
        let Form(data) = match Form::<HashMap<String, String>>::from_request(request, &state).await
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Form parse error: {}", e);
                return redirect_response(&format!("/admin/collections/{}/{}", slug, id));
            }
        };
        (data, None)
    };

    let method = form_data.remove("_method").unwrap_or_default();

    if method.eq_ignore_ascii_case("DELETE") {
        return crate::admin::handlers::collections::shared::delete_action_impl(
            &state, &slug, &id, &auth_user,
        )
        .await
        .into_response();
    }

    do_update(&state, &slug, &id, form_data, file, &auth_user).await
}
