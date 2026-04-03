//! Validation-only endpoints for collection items.
//!
//! These run the full before_validate → validate pipeline inside a rolled-back transaction,
//! returning JSON `{ valid: true }` or `{ valid: false, errors: { ... } }`.
//! Used by the `<crap-validate-form>` component to validate fields before uploading files.

/// Handler for validating a create form.
pub mod validate_create;
/// Handler for validating an update form.
pub mod validate_update;

use std::collections::HashMap;

use axum::{Extension, response::Response};
use serde_json::Value;

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::{extract_join_data_from_form, transform_select_has_many},
            shared::strip_write_denied_string_fields,
            validate::{ValidateRequest, validation_error_response_simple, values_to_string_map},
        },
    },
    core::{CollectionDefinition, auth::AuthUser},
};

pub use validate_create::validate_create;
pub use validate_update::validate_update;

/// Prepared form data and extracted join data, ready for validation.
type PreparedFormData = (HashMap<String, String>, HashMap<String, Value>);

/// Prepare form data for validation: strip denied fields, remove password,
/// transform selects, extract join data, and inject upload placeholders.
fn prepare_form_for_validation(
    state: &AdminState,
    def: &CollectionDefinition,
    auth_user: &Option<Extension<AuthUser>>,
    payload: &ValidateRequest,
    operation: &str,
) -> Result<PreparedFormData, Box<Response>> {
    let mut form_data = values_to_string_map(&payload.data);

    if let Err(_resp) =
        strip_write_denied_string_fields(state, auth_user, &def.fields, operation, &mut form_data)
    {
        return Err(Box::new(validation_error_response_simple(
            "Access check failed",
        )));
    }

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
