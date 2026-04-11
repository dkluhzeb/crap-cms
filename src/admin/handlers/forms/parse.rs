//! Form parsing: multipart and regular form extraction.

use anyhow::anyhow;
use axum::extract::{Form, FromRequest, Multipart, Request, multipart::Field};
use std::collections::HashMap;

use crate::{
    admin::AdminState,
    core::upload::{UploadedFile, UploadedFileBuilder},
};

/// Parsed form result: field data and optional uploaded file.
pub(crate) type ParsedForm = (HashMap<String, String>, Option<UploadedFile>);

/// Extract an uploaded file from a multipart field.
async fn extract_upload_field(field: Field<'_>) -> Result<Option<UploadedFile>, anyhow::Error> {
    let filename = field.file_name().unwrap_or("").to_string();
    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    let data = field
        .bytes()
        .await
        .map_err(|e| anyhow!("Failed to read file data: {}", e))?;

    if data.is_empty() {
        return Ok(None);
    }

    Ok(Some(
        UploadedFileBuilder::new(filename, content_type)
            .data(data.to_vec())
            .build(),
    ))
}

/// Parse a multipart form request, extracting form fields and an optional file upload.
pub(crate) async fn parse_multipart_form(
    request: Request,
    state: &AdminState,
) -> Result<(HashMap<String, String>, Option<UploadedFile>), anyhow::Error> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|e| anyhow!("Failed to parse multipart: {}", e))?;

    let mut form_data = HashMap::new();
    let mut file: Option<UploadedFile> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| anyhow!("Failed to read multipart field: {}", e))?
    {
        let name = field.name().unwrap_or("").to_string();

        if name == "_file" && field.file_name().is_some() {
            file = extract_upload_field(field).await?;
        } else {
            let text = field
                .text()
                .await
                .map_err(|e| anyhow!("Failed to read form field '{}': {}", name, e))?;

            form_data.insert(name, text);
        }
    }

    Ok((form_data, file))
}

/// Parse form data — multipart for upload collections, regular form otherwise.
pub(crate) async fn parse_form(
    request: Request,
    state: &AdminState,
    def: &crate::core::CollectionDefinition,
) -> Result<ParsedForm, String> {
    if def.is_upload_collection() {
        parse_multipart_form(request, state)
            .await
            .map_err(|e| format!("Multipart parse error: {}", e))
    } else {
        let form = Form::<HashMap<String, String>>::from_request(request, state)
            .await
            .map_err(|e| format!("Form parse error: {}", e))?;

        Ok((form.0, None))
    }
}
