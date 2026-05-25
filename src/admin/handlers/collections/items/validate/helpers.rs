//! Shared form-preparation helper used by both `validate_create`
//! and `validate_update` handlers.

use axum::Extension;

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::FormData,
            validate::{ValidateRequest, values_to_string_map},
        },
    },
    core::{CollectionDefinition, DocumentFields, auth::AuthUser},
};

/// Prepare form data for validation: strip denied fields, remove password,
/// inject upload placeholders, and merge form + join data into the typed
/// write payload.
pub(super) fn prepare_form_for_validation(
    _state: &AdminState,
    def: &CollectionDefinition,
    _auth_user: Option<&Extension<AuthUser>>,
    payload: &ValidateRequest,
    _operation: &str,
) -> DocumentFields {
    let mut form_data = values_to_string_map(&payload.data);

    // Field write access stripping is now handled inside service::validate_document
    // via WriteHooks::field_write_denied.

    form_data.remove("password");

    if let Some(upload_config) = &def.upload {
        for name in upload_config.system_field_names() {
            form_data.insert(name, "_pending_upload".to_string());
        }
    }

    FormData::from_raw(form_data, &def.fields).into()
}
