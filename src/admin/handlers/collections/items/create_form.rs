use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;
use std::collections::HashMap;

use crate::{
    admin::{
        AdminState,
        context::{Breadcrumb, ContextBuilder, PageType},
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, check_access_or_forbid, enrich_field_contexts,
            extract_editor_locale, forbidden, is_non_default_locale, not_found, render_or_error,
            split_sidebar_fields,
        },
    },
    core::{AuthUser, Claims},
    db::AccessResult,
};

/// GET /admin/collections/{slug}/create — show create form
pub async fn create_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug));
        }
    };

    // Check create access
    match check_access_or_forbid(&state, def.access.create.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(
                &state,
                "You don't have permission to create items in this collection",
            );
        }
        Err(resp) => return *resp,
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
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .build(),
    );

    // Evaluate display conditions (empty form data for create)
    let empty_data = json!({});
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &empty_data,
        &state.hook_runner,
        true,
    );

    if def.is_auth_collection() {
        fields.push(json!({
            "name": "password",
            "field_type": "password",
            "label": "password",
            "required": true,
            "value": "",
            "description": "set_password_description",
        }));
    }

    // Split fields into main and sidebar
    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let (_locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let mut data = ContextBuilder::new(&state, claims_ref)
        .locale_from_auth(&auth_user)
        .filter_nav_by_access(&state, &auth_user)
        .editor_locale(editor_locale.as_deref(), &state.config.locale)
        .page(PageType::CollectionCreate, "create_name")
        .page_title_name(def.singular_name())
        .collection_def(&def)
        .fields(main_fields)
        .set("sidebar_fields", json!(sidebar_fields))
        .set("editing", json!(false))
        .set("has_drafts", json!(def.has_drafts()))
        .breadcrumbs(vec![
            Breadcrumb::link("collections", "/admin/collections"),
            Breadcrumb::link(def.display_name(), format!("/admin/collections/{}", slug)),
            Breadcrumb::current("create_name").with_name(def.singular_name()),
        ])
        .merge(locale_data)
        .build();

    // Add upload context for upload collections
    if def.is_upload_collection() {
        let mut upload_ctx = json!({});
        if let Some(ref u) = def.upload
            && !u.mime_types.is_empty()
        {
            upload_ctx["accept"] = json!(u.mime_types.join(","));
        }
        data["upload"] = upload_ctx;
    }

    let data = state.hook_runner.run_before_render(data);

    render_or_error(&state, "collections/edit", &data)
}
