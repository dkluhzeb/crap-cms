//! Form parsing: multipart and regular form extraction.

use std::{
    collections::HashMap,
    error::Error,
    fmt::{self, Display, Formatter},
};

use axum::{
    extract::{Form, FromRequest, Multipart, Request, multipart::Field},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
};
use serde_json::json;

use crate::{
    admin::AdminState,
    core::{CollectionDefinition, upload::UploadedFile},
};

/// Parsed form result: field data and optional uploaded file.
pub(crate) type ParsedForm = (HashMap<String, String>, Option<UploadedFile>);

/// Why a submitted form could not be read, with the HTTP status the
/// extractor assigned — `413 Payload Too Large` when the body exceeded the
/// request size limit (an upload over the configured maximum), `400` / `415`
/// for a malformed or mistyped body.
#[derive(Debug)]
pub(crate) struct FormParseError {
    status: StatusCode,
    detail: String,
}

impl FormParseError {
    pub(crate) fn new(status: StatusCode, detail: String) -> Self {
        Self { status, detail }
    }

    /// Whether the body exceeded the request size limit.
    pub(crate) fn is_too_large(&self) -> bool {
        self.status == StatusCode::PAYLOAD_TOO_LARGE
    }
}

impl Display for FormParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.detail, self.status)
    }
}

impl Error for FormParseError {}

/// Extract an uploaded file from a multipart field.
async fn extract_upload_field(field: Field<'_>) -> Result<Option<UploadedFile>, FormParseError> {
    let filename = field.file_name().unwrap_or("").to_string();
    let content_type = field
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    let data = field
        .bytes()
        .await
        .map_err(|e| FormParseError::new(e.status(), format!("Failed to read file data: {e}")))?;

    if data.is_empty() {
        return Ok(None);
    }

    Ok(Some(UploadedFile {
        filename,
        content_type,
        data: data.to_vec(),
    }))
}

/// Collapse a list of `(key, value)` pairs into a `HashMap<String, String>`.
///
/// A key submitted once keeps its value as-is. A key submitted more than once
/// (`<select multiple>`, or any widget that repeats a name) becomes the JSON
/// array of its non-empty values — the canonical wire form of a list, which
/// every has-many reader parses — so a value containing a comma survives
/// intact. When only one of the repeated values is non-empty it stands alone,
/// and when none is, the key holds the empty string.
fn collapse_duplicates(pairs: Vec<(String, String)>) -> HashMap<String, String> {
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();

    for (name, value) in pairs {
        grouped.entry(name).or_default().push(value);
    }

    grouped
        .into_iter()
        .map(|(name, values)| (name, collapse_values(values)))
        .collect()
}

/// One key's submitted values as a single form string (see
/// [`collapse_duplicates`]).
fn collapse_values(mut values: Vec<String>) -> String {
    if values.len() == 1 {
        return values.pop().unwrap_or_default();
    }

    let mut present: Vec<String> = values.into_iter().filter(|v| !v.is_empty()).collect();

    match present.len() {
        0 => String::new(),
        1 => present.pop().unwrap_or_default(),
        _ => json!(present).to_string(),
    }
}

/// Parse a multipart form request, extracting form fields and an optional file upload.
pub(crate) async fn parse_multipart_form(
    request: Request,
    state: &AdminState,
) -> Result<ParsedForm, FormParseError> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|e| FormParseError::new(e.status(), format!("Failed to parse multipart: {e}")))?;

    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut file: Option<UploadedFile> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        FormParseError::new(e.status(), format!("Failed to read multipart field: {e}"))
    })? {
        let name = field.name().unwrap_or("").to_string();

        if name == "_file" && field.file_name().is_some() {
            file = extract_upload_field(field).await?;
            continue;
        }

        let text = field.text().await.map_err(|e| {
            FormParseError::new(
                e.status(),
                format!("Failed to read form field '{name}': {e}"),
            )
        })?;

        pairs.push((name, text));
    }

    Ok((collapse_duplicates(pairs), file))
}

/// Whether a request body is multipart form data.
fn is_multipart(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("multipart/form-data")
        })
}

/// Parse form data — multipart for an upload collection's form (the one that
/// can carry a file), a regular form otherwise. An upload collection's action
/// that posts no fields (the edit form's Unpublish) arrives URL-encoded and is
/// read as such.
pub(crate) async fn parse_form(
    request: Request,
    state: &AdminState,
    def: &CollectionDefinition,
) -> Result<ParsedForm, FormParseError> {
    if def.is_upload_collection() && is_multipart(request.headers()) {
        return parse_multipart_form(request, state).await;
    }

    // `Vec<(String, String)>` preserves every `name=value` pair, including
    // the repeated ones `<select multiple>` submits.
    let form = Form::<Vec<(String, String)>>::from_request(request, state)
        .await
        .map_err(|e| FormParseError::new(e.status(), format!("Form parse error: {e}")))?;

    Ok((collapse_duplicates(form.0), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_multipart_body_is_told_by_its_content_type() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, value.parse().unwrap());
            headers
        };

        assert!(is_multipart(&with("multipart/form-data; boundary=x")));
        assert!(is_multipart(&with("Multipart/Form-Data; boundary=x")));
        assert!(!is_multipart(&with("application/x-www-form-urlencoded")));
        assert!(!is_multipart(&HeaderMap::new()));
    }

    #[test]
    fn form_parse_error_reports_an_oversized_body() {
        let too_large = FormParseError::new(StatusCode::PAYLOAD_TOO_LARGE, "limit".into());
        assert!(too_large.is_too_large());

        let malformed = FormParseError::new(StatusCode::BAD_REQUEST, "bad".into());
        assert!(!malformed.is_too_large());
    }

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
    fn collapse_duplicates_turns_repeated_keys_into_a_json_array() {
        let pairs = vec![
            ("skills".into(), "design".into()),
            ("skills".into(), "motion".into()),
            ("skills".into(), "3d".into()),
            ("name".into(), "Taylor".into()),
        ];
        let form = collapse_duplicates(pairs);
        assert_eq!(
            form.get("skills").unwrap(),
            r#"["design","motion","3d"]"#,
            "duplicate keys from `<select multiple>` must all be kept, not truncated to the last"
        );
        assert_eq!(form.get("name").unwrap(), "Taylor");
    }

    /// Regression: repeated values were comma-joined and later comma-split,
    /// so an option value containing a comma came back as two values.
    #[test]
    fn collapse_duplicates_keeps_commas_inside_values() {
        let pairs = vec![
            ("sizes".into(), "10,5 cm".into()),
            ("sizes".into(), "12 cm".into()),
        ];
        let form = collapse_duplicates(pairs);
        assert_eq!(form.get("sizes").unwrap(), r#"["10,5 cm","12 cm"]"#);
    }

    #[test]
    fn collapse_duplicates_skips_empty_values() {
        // An empty placeholder submitted beside one real value leaves that
        // value alone rather than a one-element list.
        let pairs = vec![
            ("tags".into(), String::new()),
            ("tags".into(), "red".into()),
        ];
        let form = collapse_duplicates(pairs);
        assert_eq!(form.get("tags").unwrap(), "red");

        let pairs = vec![
            ("tags".into(), String::new()),
            ("tags".into(), String::new()),
        ];
        let form = collapse_duplicates(pairs);
        assert_eq!(form.get("tags").unwrap(), "");
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
