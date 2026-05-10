//! Shared form-preparation helper used by both `validate_create`
//! and `validate_update` handlers.

use std::collections::HashMap;

use axum::{Extension, response::Response};

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::{extract_join_data_from_form, transform_select_has_many},
            validate::{ValidateRequest, values_to_string_map},
        },
    },
    core::{CollectionDefinition, DocumentFields, auth::AuthUser},
};

/// Prepared form data and extracted join data, ready for validation.
pub(super) type PreparedFormData = (HashMap<String, String>, DocumentFields);

/// Prepare form data for validation: strip denied fields, remove password,
/// transform selects, extract join data, and inject upload placeholders.
pub(super) fn prepare_form_for_validation(
    _state: &AdminState,
    def: &CollectionDefinition,
    _auth_user: &Option<Extension<AuthUser>>,
    payload: &ValidateRequest,
    _operation: &str,
) -> Result<PreparedFormData, Box<Response>> {
    let mut form_data = values_to_string_map(&payload.data);

    // Field write access stripping is now handled inside service::validate_document
    // via WriteHooks::field_write_denied.

    form_data.remove("password");
    transform_select_has_many(&mut form_data, &def.fields);
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    if let Some(upload_config) = &def.upload {
        for name in upload_config.system_field_names() {
            form_data.insert(name, "_pending_upload".to_string());
        }
    }

    Ok((form_data, join_data))
}
