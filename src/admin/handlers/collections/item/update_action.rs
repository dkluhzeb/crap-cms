use axum::{
    Extension,
    extract::{Form, FromRequest, Path, Request, State},
    response::Response,
};
use std::collections::HashMap;

use crate::{
    admin::{
        AdminState,
        handlers::{
            collections::{
                forms::parse_multipart_form,
                shared::{delete_action_impl, do_update},
            },
            shared::redirect_response,
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
        return delete_action_impl(&state, &slug, &id, &auth_user, false, false).await;
    }

    do_update(&state, &slug, &id, form_data, file, &auth_user).await
}
