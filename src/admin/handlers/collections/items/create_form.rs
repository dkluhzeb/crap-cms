use std::collections::HashMap;

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::json;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, CollectionContext, CollectionPermissions, PageMeta,
            PageType, field::FieldContext, page::collections::CollectionCreatePage,
        },
        handlers::{
            collections::shared::{password_field, upload_form_context},
            shared::{
                EnrichOptions, HxNav, PageRequest, apply_display_conditions, build_field_contexts,
                check_access_or_forbid, collection_base, editor_locale_ctx, enrich_field_contexts,
                extract_editor_locale, forbidden, get_user_doc, is_non_default_locale, render_page,
                require_collection, split_sidebar_fields,
            },
        },
    },
    core::{AuthUser, Claims, CollectionDefinition, DocumentFields},
    db::AccessResult,
    hooks::ConditionContext,
};

/// Build, enrich, and split the field contexts for the create form.
fn prepare_create_fields(
    state: &AdminState,
    def: &CollectionDefinition,
    editor_locale: Option<&str>,
    auth_user: Option<&Extension<AuthUser>>,
) -> (Vec<FieldContext>, Vec<FieldContext>) {
    let non_default_locale = is_non_default_locale(state, editor_locale);
    let empty: HashMap<String, String> = HashMap::new();

    let mut fields = build_field_contexts(&def.fields, &empty, &empty, true, non_default_locale);

    let locale_ctx = editor_locale_ctx(&state.config.locale, editor_locale);
    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &DocumentFields::new(),
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .user(get_user_doc(auth_user))
            .locale_ctx(locale_ctx.as_ref())
            .build(),
    );

    let cond_ctx = ConditionContext {
        collection: &def.slug,
        operation: "create",
        user: get_user_doc(auth_user),
        ui_locale: auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: editor_locale,
        options: None,
    };

    apply_display_conditions(
        &mut fields,
        &def.fields,
        &json!({}),
        &state.infra.hook_runner,
        true,
        &cond_ctx,
    );

    if def.is_auth_collection() {
        fields.push(password_field(true));
    }

    split_sidebar_fields(fields)
}

/// GET /admin/collections/{slug}/create — show create form
pub async fn create_form(
    State(state): State<AdminState>,
    hx: HxNav,
    Path(slug): Path<String>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match require_collection(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    match check_access_or_forbid(
        &state,
        def.access.create.as_ref(),
        auth_user.as_ref(),
        None,
        None,
        "create",
        &slug,
    ) {
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
    let (main_fields, sidebar_fields) =
        prepare_create_fields(&state, &def, editor_locale.as_deref(), auth_user.as_ref());

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let mut breadcrumbs = collection_base(&def, &slug);
    breadcrumbs.push(Breadcrumb::current("create_name").with_name(def.singular_name()));

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
        PageMeta::new(PageType::CollectionCreate, "create_name")
            .with_title_name(def.singular_name()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let upload = def
        .is_upload_collection()
        .then(|| upload_form_context(&def, state.config.upload.max_file_size));

    let perms = CollectionPermissions::for_user(&state, &def, auth_user.as_ref());

    let ctx = CollectionCreatePage {
        base,
        collection: CollectionContext::from_def(&def),
        perms,
        fields: main_fields,
        sidebar_fields,
        editing: false,
        has_drafts: def.has_drafts(),
        upload,
    };

    render_page(
        &state,
        PageRequest::new(hx, auth_user.as_ref()),
        "collections/edit",
        &ctx,
    )
    .await
}
