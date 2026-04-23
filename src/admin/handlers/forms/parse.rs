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

/// Collapse a list of `(key, value)` pairs into a `HashMap<String, String>`,
/// comma-joining every value for keys that appear more than once.
///
/// `<select multiple>` and any other widget that submits the same name more
/// than once would otherwise be silently truncated to the last value by
/// `HashMap`. Joining with commas matches the input shape that
/// `transform_select_has_many` already expects for `has_many` fields.
fn collapse_duplicates(pairs: Vec<(String, String)>) -> HashMap<String, String> {
    let mut form_data: HashMap<String, String> = HashMap::new();

    for (name, value) in pairs {
        match form_data.entry(name) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let existing = e.get_mut();

                if !existing.is_empty() && !value.is_empty() {
                    existing.push(',');
                }

                existing.push_str(&value);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(value);
            }
        }
    }

    form_data
}

/// Parse a multipart form request, extracting form fields and an optional file upload.
pub(crate) async fn parse_multipart_form(
    request: Request,
    state: &AdminState,
) -> Result<(HashMap<String, String>, Option<UploadedFile>), anyhow::Error> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|e| anyhow!("Failed to parse multipart: {}", e))?;

    let mut pairs: Vec<(String, String)> = Vec::new();
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

            pairs.push((name, text));
        }
    }

    Ok((collapse_duplicates(pairs), file))
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
        // `Vec<(String, String)>` preserves every `name=value` pair, including
        // duplicates that `HashMap<String, String>` would silently drop — the
        // `<select multiple>` / has_many failure mode prior to this change.
        let form = Form::<Vec<(String, String)>>::from_request(request, state)
            .await
            .map_err(|e| format!("Form parse error: {}", e))?;

        Ok((collapse_duplicates(form.0), None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_duplicates_preserves_single_values() {
        let pairs = vec![
            ("name".into(), "Alex".into()),
            ("email".into(), "alex@example.com".into()),
        ];
        let form = collapse_duplicates(pairs);
        assert_eq!(form.get("name").unwrap(), "Alex");
        assert_eq!(form.get("email").unwrap(), "alex@example.com");
    }

    #[test]
    fn collapse_duplicates_joins_multiple_values() {
        let pairs = vec![
            ("skills".into(), "design".into()),
            ("skills".into(), "motion".into()),
            ("skills".into(), "3d".into()),
            ("name".into(), "Taylor".into()),
        ];
        let form = collapse_duplicates(pairs);
        assert_eq!(
            form.get("skills").unwrap(),
            "design,motion,3d",
            "duplicate keys from `<select multiple>` must be joined, not truncated to the last"
        );
        assert_eq!(form.get("name").unwrap(), "Taylor");
    }

    #[test]
    fn collapse_duplicates_skips_empty_values_in_join() {
        // Browsers never send empty values for `<select multiple>`, but empty
        // placeholder options on single selects may submit one. Don't leave a
        // leading comma that would later be parsed as an empty value.
        let pairs = vec![
            ("tags".into(), String::new()),
            ("tags".into(), "red".into()),
        ];
        let form = collapse_duplicates(pairs);
        assert_eq!(form.get("tags").unwrap(), "red");
    }

    #[test]
    fn collapse_duplicates_single_empty_value_kept() {
        // A single empty submission must be stored as-is so downstream
        // "field is missing vs field is empty" logic still works.
        let pairs = vec![("optional".into(), String::new())];
        let form = collapse_duplicates(pairs);
        assert_eq!(form.get("optional").unwrap(), "");
    }
}
