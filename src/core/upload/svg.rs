//! SVG content scanning: an uploaded SVG is stored only when it cannot load or
//! run anything outside the file.

use std::{str, sync::LazyLock};

use anyhow::{Context as _, Result, bail};
use regex::{Captures, Regex};

/// Best-effort check whether an uploaded file is an SVG. `infer` does not
/// classify text-based formats, so we also peek at the raw bytes.
pub(super) fn is_svg(effective_mime: &str, data: &[u8]) -> bool {
    if effective_mime == "image/svg+xml" {
        return true;
    }

    // Look only at the first 1 kB — enough to find the XML / <svg> prolog
    // without paying for a full scan on non-SVG content.
    let head = &data[..data.len().min(1024)];
    let prefix = str::from_utf8(head).unwrap_or("");
    let trimmed = prefix.trim_start();
    let lower = trimmed.to_ascii_lowercase();

    lower.starts_with("<?xml") && lower.contains("<svg") || lower.starts_with("<svg")
}

/// Every quoted `href` / `xlink:href` attribute value (group 1), and every
/// `url(…)` argument in a style or paint attribute (group 2) — the two places
/// an SVG names something for the renderer to fetch.
static REFERENCE_VALUES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:(?:xlink:)?href\s*=\s*["']([^"']*)["'])|(?:url\(\s*["']?([^"')]*))"#)
        .expect("static regex")
});

/// A value that starts with a URL scheme or a protocol-relative `//`; group 1
/// is the scheme. `data:` is the only scheme an uploaded asset legitimately
/// embeds.
static LEADING_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?://|([a-z][a-z0-9+.\-]*):)").expect("static regex"));

/// Numeric character references plus the named ones that can spell a scheme.
static CHARACTER_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)&(?:#x([0-9a-f]+)|#([0-9]+)|(colon|sol|tab|newline));").expect("static regex")
});

/// An inline event handler (`onload=`, `onclick=`, …) runs script without a
/// `<script>` element.
static EVENT_HANDLER_ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)[\s"'/]on[a-z]+\s*="#).expect("static regex"));

/// The value as a renderer reads it: numeric and the scheme-relevant named
/// character references decoded, ASCII controls and whitespace removed —
/// browsers strip those inside a URL, so `java&#9;script:` is `javascript:`.
fn normalized_reference(raw: &str) -> String {
    let decoded = CHARACTER_REFERENCE.replace_all(raw, |caps: &Captures<'_>| {
        let code = caps
            .get(1)
            .and_then(|m| u32::from_str_radix(m.as_str(), 16).ok())
            .or_else(|| caps.get(2).and_then(|m| m.as_str().parse().ok()))
            .or_else(
                || match caps.get(3).map(|m| m.as_str().to_ascii_lowercase()) {
                    Some(name) if name == "colon" => Some(u32::from(':')),
                    Some(name) if name == "sol" => Some(u32::from('/')),
                    Some(_) => Some(u32::from(' ')),
                    None => None,
                },
            );

        code.and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });

    decoded
        .chars()
        .filter(|c| !c.is_ascii_control() && !c.is_whitespace())
        .collect()
}

/// A reference that makes a renderer fetch (or execute) something outside the
/// file: any scheme except `data:`, or a protocol-relative URL. Fragments,
/// relative paths and `data:` URIs stay inside the file.
fn external_reference(text: &str) -> Option<String> {
    REFERENCE_VALUES.captures_iter(text).find_map(|caps| {
        let raw = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map_or("", |m| m.as_str());
        let value = normalized_reference(raw).to_ascii_lowercase();
        let scheme = LEADING_SCHEME.captures(&value)?;
        let is_data = scheme.get(1).is_some_and(|m| m.as_str() == "data");

        (!is_data).then(|| raw.chars().take(60).collect())
    })
}

/// Reject an SVG that could load or run anything outside the file: an
/// external reference, an inline event handler, a CSS `@import`, a `<script>`
/// element, or the classic XXE indicators — a DOCTYPE declaration (gateway to
/// external/general entity abuse) or an explicit ENTITY declaration.
/// Case-insensitive: XML tags are case-sensitive but the attack strings are
/// well-known ASCII tokens a scan must catch in any case.
pub(super) fn validate_svg_content(data: &[u8]) -> Result<()> {
    let text = str::from_utf8(data).context("SVG is not valid UTF-8")?;
    let lower = text.to_ascii_lowercase();

    // A remote reference turns every render into a request the uploader chose:
    // a tracking beacon at best, `javascript:` at worst. Fragments, relative
    // paths and `data:` URIs stay inside the file and are allowed; `mailto:`
    // and `tel:` links are refused with the rest — an uploaded image is a
    // static asset, not a document with links.
    if let Some(reference) = external_reference(text) {
        bail!(
            "SVG references an external resource ({reference}…). Remove it — \
             uploaded images must not load or execute anything outside the file \
             (only fragments, relative paths and data: URIs are allowed)."
        );
    }

    if EVENT_HANDLER_ATTRIBUTE.is_match(text) {
        bail!(
            "SVG contains an inline event handler (on…= attribute). Remove it — \
             uploaded images are static assets and must not run script."
        );
    }

    if lower.contains("@import") {
        bail!(
            "SVG contains a CSS @import. Remove it — uploaded images must not \
             load anything outside the file."
        );
    }

    // An SVG is served as a download under a sandbox CSP, so a script inside
    // one cannot run from the serve route. It would still run if the file is
    // ever opened directly from disk or re-served by a consumer that does not
    // reproduce those headers, and no legitimate uploaded asset needs one.
    if lower.contains("<script") {
        bail!(
            "SVG contains a <script> element. Remove it — uploaded images are \
             static assets, and a scripted SVG is a stored-XSS vector for any \
             consumer that renders it inline."
        );
    }

    if lower.contains("<!doctype") {
        bail!(
            "SVG contains a <!DOCTYPE> declaration. Remove it — DOCTYPE is \
             an XXE gateway and not required for SVGs that render in any \
             modern browser."
        );
    }

    if lower.contains("<!entity") {
        bail!("SVG contains an <!ENTITY> declaration — reject as a potential XXE vector.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_svg_recognises_svg_mime() {
        assert!(is_svg("image/svg+xml", b""));
    }

    #[test]
    fn is_svg_recognises_raw_svg_prolog() {
        assert!(is_svg(
            "application/octet-stream",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ));
        assert!(is_svg(
            "application/octet-stream",
            b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ));
    }

    #[test]
    fn is_svg_rejects_non_svg_content() {
        assert!(!is_svg("image/png", &[0x89, 0x50, 0x4E, 0x47]));
        assert!(!is_svg("text/html", b"<html><body></body></html>"));
    }

    #[test]
    fn svg_scan_accepts_clean_svg() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="10" height="10" fill="red"/>
        </svg>"#;
        assert!(validate_svg_content(svg).is_ok());
    }

    /// Entity-encoded or whitespace-split schemes are what a renderer sees
    /// after decoding, so the scan judges the decoded value.
    #[test]
    fn svg_scan_decodes_the_reference_before_judging_it() {
        for href in [
            "&#x6a;avascript:alert(1)",
            "&#104;ttps://evil.example/x.png",
            "java&#9;script:alert(1)",
            "javascript&colon;alert(1)",
            " \n https://evil.example/x.png",
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg"><a href="{href}"><text>x</text></a></svg>"#
            );
            assert!(
                validate_svg_content(svg.as_bytes()).is_err(),
                "{href} must be rejected"
            );
        }
    }

    /// Script runs from an event-handler attribute without any `<script>`
    /// element, and a stylesheet import or a `url(…)` paint reference fetches
    /// from outside the file.
    #[test]
    fn svg_scan_rejects_event_handlers_and_style_fetches() {
        let handler = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#;
        assert!(validate_svg_content(handler).is_err());

        let import = br#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import url(https://evil.example/a.css);</style></svg>"#;
        assert!(validate_svg_content(import).is_err());

        let paint = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(https://evil.example/p.svg#g)"/></svg>"#;
        assert!(validate_svg_content(paint).is_err());

        let local_paint = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(#grad)" font-family="Onyx"/></svg>"#;
        assert!(validate_svg_content(local_paint).is_ok());
    }

    #[test]
    fn svg_scan_rejects_external_href() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="https://evil.example/pixel.png"/></svg>"#;
        let err = validate_svg_content(svg).expect_err("remote image must be rejected");
        assert!(err.to_string().contains("external resource"), "{err}");

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><a href="javascript:alert(1)"><text>x</text></a></svg>"#;
        assert!(validate_svg_content(svg).is_err());

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><image href = '//cdn.example/x.png'/></svg>"#;
        assert!(validate_svg_content(svg).is_err());
    }

    /// The `xmlns:xlink` declaration, fragment references and embedded
    /// `data:` images are the legitimate shapes and must keep passing.
    #[test]
    fn svg_scan_accepts_internal_and_data_hrefs() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><defs><circle id="c" r="1"/></defs><use xlink:href="#c"/><use href="#c"/><image href="data:image/png;base64,iVBORw0KGgo="/><image href="logo.png"/></svg>"##;
        assert!(validate_svg_content(svg).is_ok());
    }

    #[test]
    fn svg_scan_rejects_script_element() {
        let payload = br#"<svg xmlns="http://www.w3.org/2000/svg">
            <script>alert(document.domain)</script>
        </svg>"#;
        let err = validate_svg_content(payload).unwrap_err().to_string();
        assert!(err.contains("<script>"), "unexpected error: {err}");
    }

    #[test]
    fn svg_scan_rejects_script_element_in_any_case() {
        let payload = b"<svg><SCRIPT type=\"text/javascript\">x()</SCRIPT></svg>";
        assert!(validate_svg_content(payload).is_err());
    }

    #[test]
    fn svg_scan_rejects_doctype() {
        let payload = br#"<?xml version="1.0"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg"/>"#;
        let err = validate_svg_content(payload).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("doctype"));
    }

    #[test]
    fn svg_scan_rejects_classic_xxe_payload() {
        // Textbook SVG XXE: DOCTYPE + ENTITY + use-of-entity to
        // exfiltrate a local file through a text node.
        let payload = br#"<?xml version="1.0"?>
<!DOCTYPE svg [
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<svg xmlns="http://www.w3.org/2000/svg"><text>&xxe;</text></svg>"#;
        assert!(validate_svg_content(payload).is_err());
    }

    #[test]
    fn svg_scan_rejects_entity_even_without_doctype() {
        // Some XML parsers accept inline entity declarations even without
        // a DOCTYPE. Belt-and-braces: catch both markers independently.
        let payload = br#"<svg xmlns="http://www.w3.org/2000/svg">
            <!ENTITY evil SYSTEM "http://attacker.example/beacon"/>
        </svg>"#;
        assert!(validate_svg_content(payload).is_err());
    }

    #[test]
    fn svg_scan_is_case_insensitive() {
        // Attackers can vary case to try to bypass a naive scan. Reject
        // the lowercase form too.
        let payload = b"<!doctype svg><svg/>";
        assert!(validate_svg_content(payload).is_err());
    }
}
