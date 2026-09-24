//! Delivering a served upload's bytes once the gate has passed: format-variant
//! negotiation, the local and remote backends, and the shared headers.

use std::path;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::{
    admin::{
        AdminState,
        handlers::uploads::{
            headers::{
                ConditionalHeaders, ServeHeaders, build_serve_request, extract_conditional_headers,
            },
            remote::serve_remote,
        },
    },
    core::upload::CollectionUpload,
};

/// What one resolved serve request asks for: which file, under which cache
/// policy, and which negotiated image formats the viewer accepts.
pub(super) struct ServeRequest<'a> {
    pub collection_slug: &'a str,
    pub filename: &'a str,
    pub cache_control: &'a str,
    /// The collection's upload config — which decides whether format variants
    /// exist to negotiate at all. `None` for a collection without uploads.
    pub upload: Option<&'a CollectionUpload>,
    pub accepts_avif: bool,
    pub accepts_webp: bool,
}

/// Whether a negotiated-variant response actually served the variant. A miss
/// or a backend failure falls through to the next candidate — and finally to
/// the original file — rather than failing the whole request.
fn variant_served(status: StatusCode) -> bool {
    !status.is_server_error() && status != StatusCode::NOT_FOUND
}

/// Serve the best negotiated variant of `req`, or `None` when none of them is
/// available on this backend.
async fn serve_variant(
    state: &AdminState,
    req: &ServeRequest<'_>,
    conditional: &ConditionalHeaders,
) -> Option<Response> {
    let storage = &state.infra.storage;

    for (variant_name, variant_mime) in negotiate_variants(req) {
        let variant_key = format!("{}/{variant_name}", req.collection_slug);

        let headers = ServeHeaders::new(variant_mime, req.cache_control)
            .varied(true)
            .stored_name(Some(&variant_name));

        let Some(local_path) = storage.local_path(&variant_key) else {
            let response = serve_remote(storage, &variant_key, conditional, &headers).await;

            if variant_served(response.status()) {
                return Some(response);
            }

            continue;
        };

        if local_path.exists() {
            return Some(serve_local(&local_path, conditional, &headers).await);
        }
    }

    None
}

/// Serve the requested file — its best negotiated variant when one exists,
/// the file itself otherwise.
pub(super) async fn serve_file(
    state: &AdminState,
    req: &ServeRequest<'_>,
    original_request: Request<Body>,
) -> Response {
    // Conditional / range headers are answered by `ServeFile` on the local
    // path and by the remote path itself; both need them off the original.
    let conditional = extract_conditional_headers(&original_request);

    if let Some(response) = serve_variant(state, req, &conditional).await {
        return response;
    }

    let storage = &state.infra.storage;
    let original_key = format!("{}/{}", req.collection_slug, req.filename);

    let requested_mime = mime_guess::from_path(req.filename)
        .first_or_octet_stream()
        .to_string();

    let headers = ServeHeaders::new(&requested_mime, req.cache_control)
        .varied(varies_by_accept(req))
        .stored_name(Some(req.filename));

    let Some(local_path) = storage.local_path(&original_key) else {
        return serve_remote(storage, &original_key, &conditional, &headers).await;
    };

    if !local_path.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }

    serve_local(&local_path, &conditional, &headers).await
}

/// Whether the response for `req` depends on the viewer's `Accept` header —
/// i.e. whether the upload pipeline writes format variants for this file.
///
/// The pipeline writes a `.webp` / `.avif` variant beside each generated
/// *size* file (`{stem}_{size}.{ext}`) for every configured format — never
/// beside the original. So nothing is negotiated for a collection without
/// `format_options`, for a non-image, or for a file that isn't one of the
/// collection's size files: probing there only costs a storage lookup (two
/// round-trips on a remote backend) that can never hit.
fn varies_by_accept(req: &ServeRequest<'_>) -> bool {
    let Some(upload) = req.upload else {
        return false;
    };

    let is_image = mime_guess::from_path(req.filename)
        .first_or_octet_stream()
        .to_string()
        .starts_with("image/");

    is_image && !upload.format_variants().is_empty() && is_size_file(req.filename, upload)
}

/// The variant formats worth looking for, in preference order (AVIF first,
/// then WebP): the ones the viewer accepts that the upload pipeline writes for
/// the requested file (see [`varies_by_accept`]).
fn negotiable_formats(req: &ServeRequest<'_>) -> Vec<(&'static str, &'static str)> {
    let Some(upload) = req.upload.filter(|_| varies_by_accept(req)) else {
        return Vec::new();
    };

    let mut formats = Vec::new();

    if req.accepts_avif && upload.format_options.avif.is_some() {
        formats.push(("avif", "image/avif"));
    }

    if req.accepts_webp && upload.format_options.webp.is_some() {
        formats.push(("webp", "image/webp"));
    }

    formats
}

/// Whether `filename` is named like one of the collection's generated size
/// files — its stem ends in `_{size}`.
fn is_size_file(filename: &str, upload: &CollectionUpload) -> bool {
    let Some((stem, _)) = filename.rsplit_once('.') else {
        return false;
    };

    upload.image_sizes.iter().any(|size| {
        stem.strip_suffix(size.name.as_str())
            .is_some_and(|rest| rest.ends_with('_'))
    })
}

/// The candidate variant filenames for `req`, with their content types, in
/// preference order. Empty when nothing is negotiable (see
/// [`negotiable_formats`]).
fn negotiate_variants(req: &ServeRequest<'_>) -> Vec<(String, &'static str)> {
    let Some((stem, _)) = req.filename.rsplit_once('.') else {
        return Vec::new();
    };

    negotiable_formats(req)
        .into_iter()
        .map(|(ext, mime)| (format!("{stem}.{ext}"), mime))
        .collect()
}

/// Serve a local file via `tower_http::services::ServeFile`, which answers
/// Range, `ETag`, `Last-Modified` and conditional GETs itself, and then apply
/// the headers every served upload carries.
async fn serve_local(
    path: &path::Path,
    conditional: &ConditionalHeaders,
    headers: &ServeHeaders<'_>,
) -> Response {
    let service = ServeFile::new(path);

    let mut response = match service.oneshot(build_serve_request(conditional)).await {
        Ok(r) => r.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    headers.apply(&mut response);
    response
}

#[cfg(test)]
mod tests {
    use std::fs;

    use axum::http::header::{
        ACCEPT_RANGES, CONTENT_DISPOSITION, CONTENT_RANGE, CONTENT_SECURITY_POLICY, VARY,
    };

    use super::*;
    use crate::core::upload::{FormatQuality, ImageSizeBuilder};

    /// A request that carries no range and no validators.
    fn no_conditions() -> ConditionalHeaders {
        ConditionalHeaders {
            range: None,
            if_none_match: None,
            if_modified_since: None,
        }
    }

    fn disposition_of(response: &Response) -> &str {
        response
            .headers()
            .get(CONTENT_DISPOSITION)
            .expect("a disposition is always set")
            .to_str()
            .expect("a valid header value")
    }

    /// An upload config with a `thumbnail` size and both formats.
    fn formats_upload() -> CollectionUpload {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(10)
                .height(10)
                .build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, false));
        upload.format_options.avif = Some(FormatQuality::new(60, false));
        upload
    }

    fn request<'a>(
        filename: &'a str,
        upload: Option<&'a CollectionUpload>,
        avif: bool,
        webp: bool,
    ) -> ServeRequest<'a> {
        ServeRequest {
            collection_slug: "media",
            filename,
            cache_control: "public",
            upload,
            accepts_avif: avif,
            accepts_webp: webp,
        }
    }

    fn names(req: &ServeRequest<'_>) -> Vec<String> {
        negotiate_variants(req)
            .into_iter()
            .map(|(n, _)| n)
            .collect()
    }

    #[test]
    fn negotiate_no_accept_returns_empty() {
        let upload = formats_upload();
        let req = request("abc_photo_thumbnail.jpg", Some(&upload), false, false);
        assert!(negotiate_variants(&req).is_empty());
    }

    #[test]
    fn negotiate_prefers_avif_over_webp_for_a_size_file() {
        let upload = formats_upload();
        let req = request("abc_photo_thumbnail.jpg", Some(&upload), true, true);

        assert_eq!(
            negotiate_variants(&req),
            vec![
                ("abc_photo_thumbnail.avif".to_string(), "image/avif"),
                ("abc_photo_thumbnail.webp".to_string(), "image/webp"),
            ]
        );
    }

    /// Regression: the original of an image was negotiated too, though the
    /// pipeline only ever writes variants beside size files — every request
    /// for an original paid a storage probe that could never hit.
    #[test]
    fn negotiate_skips_originals() {
        let upload = formats_upload();
        let req = request("abc_photo.jpg", Some(&upload), true, true);
        assert!(negotiate_variants(&req).is_empty());

        // A name merely *containing* the size name is not a size file.
        let req = request("abc_photothumbnail.jpg", Some(&upload), true, true);
        assert!(negotiate_variants(&req).is_empty());
    }

    /// Regression: a collection without `format_options` was probed for
    /// variants it can never have.
    #[test]
    fn negotiate_needs_configured_formats() {
        let mut upload = formats_upload();
        upload.format_options.avif = None;

        let req = request("abc_photo_thumbnail.jpg", Some(&upload), true, true);
        assert_eq!(names(&req), vec!["abc_photo_thumbnail.webp".to_string()]);

        upload.format_options.webp = None;
        let req = request("abc_photo_thumbnail.jpg", Some(&upload), true, true);
        assert!(negotiate_variants(&req).is_empty());

        let req = request("abc_photo_thumbnail.jpg", None, true, true);
        assert!(negotiate_variants(&req).is_empty());
    }

    /// `Vary: Accept` follows whether the file HAS variants, not whether this
    /// viewer asked for one — a cache must not replay a no-Accept response of
    /// a size file to a viewer that accepts WebP.
    #[test]
    fn vary_depends_on_variants_existing_not_on_the_viewer() {
        let upload = formats_upload();

        assert!(varies_by_accept(&request(
            "abc_photo_thumbnail.jpg",
            Some(&upload),
            false,
            false
        )));
        assert!(!varies_by_accept(&request(
            "abc_photo.jpg",
            Some(&upload),
            true,
            true
        )));
    }

    #[test]
    fn negotiate_non_image_returns_empty() {
        let upload = formats_upload();
        let req = request("document_thumbnail.pdf", Some(&upload), true, true);
        assert!(negotiate_variants(&req).is_empty());
    }

    #[test]
    fn negotiate_no_extension_returns_empty() {
        let upload = formats_upload();
        let req = request("noext_thumbnail", Some(&upload), true, true);
        assert!(negotiate_variants(&req).is_empty());
    }

    #[test]
    fn negotiate_keeps_a_dotted_stem() {
        let upload = formats_upload();
        let req = request("my.photo.v2_thumbnail.png", Some(&upload), false, true);
        assert_eq!(names(&req), vec!["my.photo.v2_thumbnail.webp".to_string()]);
    }

    #[tokio::test]
    async fn serve_local_image_disposition_inline() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.png");
        fs::write(&path, b"fake png").unwrap();

        let headers = ServeHeaders::new("image/png", "public");
        let resp = serve_local(&path, &no_conditions(), &headers).await;

        assert_eq!(disposition_of(&resp), "inline");
    }

    #[tokio::test]
    async fn serve_local_pdf_disposition_attachment() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.pdf");
        fs::write(&path, b"fake pdf").unwrap();

        let headers =
            ServeHeaders::new("application/pdf", "public").stored_name(Some("abcdefghij_test.pdf"));
        let resp = serve_local(&path, &no_conditions(), &headers).await;

        assert_eq!(disposition_of(&resp), "attachment; filename=\"test.pdf\"");
    }

    #[tokio::test]
    async fn serve_local_varied_sets_vary() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.jpg");
        fs::write(&path, b"fake jpg").unwrap();

        let headers = ServeHeaders::new("image/jpeg", "public").varied(true);
        let resp = serve_local(&path, &no_conditions(), &headers).await;

        assert_eq!(resp.headers().get(VARY).unwrap(), "Accept");
    }

    #[tokio::test]
    async fn serve_local_no_vary_when_not_set() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, b"hello").unwrap();

        let resp = serve_local(
            &path,
            &no_conditions(),
            &ServeHeaders::new("text/plain", "no-cache"),
        )
        .await;

        // ServeFile may set Vary internally, but we don't set it
        assert!(!resp.headers().get_all(VARY).iter().any(|v| v == "Accept"));
    }

    #[tokio::test]
    async fn serve_local_svg_attachment_and_csp() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.svg");
        fs::write(&path, b"<svg></svg>").unwrap();

        let resp = serve_local(
            &path,
            &no_conditions(),
            &ServeHeaders::new("image/svg+xml", "public"),
        )
        .await;

        assert_eq!(disposition_of(&resp), "attachment");
        let csp = resp
            .headers()
            .get(CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(csp, "sandbox; default-src 'none'");
    }

    /// Every backend advertises ranged reads now, the local one included.
    #[tokio::test]
    async fn serve_local_advertises_range_support() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.bin");
        fs::write(&path, b"0123456789").unwrap();

        let resp = serve_local(
            &path,
            &no_conditions(),
            &ServeHeaders::new("application/octet-stream", "public"),
        )
        .await;

        assert_eq!(resp.headers().get(ACCEPT_RANGES).unwrap(), "bytes");
    }

    /// The local backend answers a range itself; the shared headers must not
    /// disturb the `206` it produced.
    #[tokio::test]
    async fn serve_local_answers_a_range_with_206() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.bin");
        fs::write(&path, b"0123456789").unwrap();

        let conditional = ConditionalHeaders {
            range: Some("bytes=2-4".parse().unwrap()),
            if_none_match: None,
            if_modified_since: None,
        };

        let resp = serve_local(
            &path,
            &conditional,
            &ServeHeaders::new("application/octet-stream", "public"),
        )
        .await;

        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers().get(CONTENT_RANGE).unwrap(), "bytes 2-4/10");
    }

    /// A variant that is missing (404) or whose backend failed (503) must fall
    /// through to the next candidate, never end the request.
    #[test]
    fn only_a_real_variant_response_ends_negotiation() {
        assert!(variant_served(StatusCode::OK));
        assert!(variant_served(StatusCode::PARTIAL_CONTENT));
        assert!(variant_served(StatusCode::NOT_MODIFIED));
        assert!(variant_served(StatusCode::RANGE_NOT_SATISFIABLE));

        assert!(!variant_served(StatusCode::NOT_FOUND));
        assert!(!variant_served(StatusCode::SERVICE_UNAVAILABLE));
    }
}
