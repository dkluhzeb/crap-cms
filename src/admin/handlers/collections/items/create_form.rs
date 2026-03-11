use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;

use crate::admin::context::{Breadcrumb, ContextBuilder, PageType};
use crate::admin::AdminState;
use crate::core::auth::{AuthUser, Claims};

use crate::admin::handlers::shared::{
    apply_display_conditions, build_field_contexts, build_locale_template_data,
    check_access_or_forbid, enrich_field_contexts, extract_editor_locale, forbidden,
    is_non_default_locale, not_found, render_or_error, split_sidebar_fields,
};
use crate::db::query::AccessResult;

/// GET /admin/collections/{slug}/create — show create form
pub async fn create_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: axum::http::HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> impl IntoResponse {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug)).into_response()
        }
    };

    // Check create access
    match check_access_or_forbid(&state, def.access.create.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(
                &state,
                "You don't have permission to create items in this collection",
            )
            .into_response()
        }
        Err(resp) => return resp,
        _ => {}
    }

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let non_default_locale = is_non_default_locale(&state, editor_locale.as_deref());
    let mut fields = build_field_contexts(
        &def.fields,
        &HashMap::new(),
        &HashMap::new(),
        true,
        non_default_locale,
    );

    // Enrich relationship and array fields
    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &HashMap::new(),
        &state,
        true,
        non_default_locale,
        &HashMap::new(),
        None,
    );

    // Evaluate display conditions (empty form data for create)
    let empty_data = serde_json::json!({});
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &empty_data,
        &state.hook_runner,
        true,
    );

    if def.is_auth_collection() {
        fields.push(serde_json::json!({
            "name": "password",
            "field_type": "password",
            "label": "Password",
            "required": true,
            "value": "",
            "description": "Set the user's password",
        }));
    }

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let (_locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let claims_ref = claims.as_ref().map(|Extension(c)| c);
    let mut data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(
            PageType::CollectionCreate,
            format!("Create {}", def.singular_name()),
        )
        .set(
            "page_title",
            serde_json::json!(format!("Create {}", def.singular_name())),
        )
        .collection_def(&def)
        .fields(main_fields)
        .set("sidebar_fields", serde_json::json!(sidebar_fields))
        .set("editing", serde_json::json!(false))
        .set("has_drafts", serde_json::json!(def.has_drafts()))
        .breadcrumbs(vec![
            Breadcrumb::link("Collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current(format!("Create {}", def.singular_name())),
        ])
        .merge(locale_data)
        .build();

    // Add upload context for upload collections
    if def.is_upload_collection() {
        let mut upload_ctx = serde_json::json!({});
        if let Some(ref u) = def.upload {
            if !u.mime_types.is_empty() {
                upload_ctx["accept"] = serde_json::json!(u.mime_types.join(","));
            }
        }
        data["upload"] = upload_ctx;
    }

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data).into_response()
}
