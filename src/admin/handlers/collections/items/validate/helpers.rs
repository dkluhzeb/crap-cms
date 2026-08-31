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
    core::{CollectionDefinition, DocumentFields, FieldType, auth::AuthUser},
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
    prepare_form_for_validation_inner(def, payload)
}

/// State-free body of [`prepare_form_for_validation`] (the state/auth/
/// operation params are currently unused there) — unit-testable.
fn prepare_form_for_validation_inner(
    def: &CollectionDefinition,
    payload: &ValidateRequest,
) -> DocumentFields {
    let mut form_data = values_to_string_map(&payload.data);

    // Field write access stripping is now handled inside service::validate_document
    // via WriteHooks::field_write_denied.

    form_data.remove("password");

    // Upload system fields are server-managed (filled by the upload
    // pipeline), so the pre-upload dry-run must not fail on them:
    // - string-typed fields (filename, url, mime_type, …) get a placeholder
    //   so their `required` constraints pass;
    // - NUMBER-typed fields (width/height/filesize/focal/size variants) are
    //   REMOVED instead — an absent number skips numeric validation, while
    //   the old string placeholder trips the strict non-numeric rejection
    //   (`validation.invalid_number` on every media metadata field).
    if let Some(upload_config) = &def.upload {
        for name in upload_config.system_field_names() {
            let is_number = def
                .fields
                .iter()
                .find(|f| f.name == name)
                .is_some_and(|f| f.field_type == FieldType::Number);

            if is_number {
                form_data.remove(&name);
            } else {
                form_data.insert(name, "_pending_upload".to_string());
            }
        }
    }

    FormData::from_raw(form_data, &def.fields).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, upload::CollectionUpload};

    fn media_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.fields = vec![
            FieldDefinition::builder("filename", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("url", FieldType::Text).build(),
            FieldDefinition::builder("width", FieldType::Number).build(),
            FieldDefinition::builder("height", FieldType::Number).build(),
            FieldDefinition::builder("filesize", FieldType::Number).build(),
            FieldDefinition::builder("caption", FieldType::Text).build(),
        ];
        def.upload = Some(CollectionUpload::new());
        def
    }

    /// Regression: the pre-upload validate dry-run injected the string
    /// placeholder `_pending_upload` into EVERY upload system field. Once
    /// Number fields started rejecting non-numeric input, that made every
    /// media validate fail with `validation.invalid_number` on
    /// width/height/filesize/focal/size fields — the admin upload form
    /// became unsubmittable. Number-typed system fields must be OMITTED
    /// from the dry-run payload; string-typed ones keep the placeholder.
    #[test]
    fn upload_placeholders_skip_number_system_fields() {
        let def = media_def();
        let payload = ValidateRequest {
            data: DocumentFields::from_iter([(
                "caption".to_string(),
                serde_json::Value::String("hi".to_string()),
            )]),
            draft: false,
            locale: None,
        };

        let prepared = prepare_form_for_validation_inner(&def, &payload);

        assert_eq!(
            prepared.get("filename").and_then(|v| v.as_str()),
            Some("_pending_upload"),
            "required string system fields keep the placeholder"
        );
        for numeric in ["width", "height", "filesize"] {
            let v = prepared.get(numeric);
            assert!(
                v.is_none_or(|v| v.as_str().is_none_or(|s| s != "_pending_upload")),
                "number system field `{numeric}` must not carry the string placeholder, got {v:?}"
            );
        }
        assert_eq!(
            prepared.get("caption").and_then(|v| v.as_str()),
            Some("hi"),
            "user fields pass through"
        );
    }
}
