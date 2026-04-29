//! Source-version header helpers for extracted overlay files.
//!
//! When `crap-cms templates extract` copies an embedded default into the
//! user's config dir, it prepends a comment line marking which crap-cms
//! version the file was extracted from. `crap-cms overlay status` parses
//! this header back out to detect drift between the user's overlay and
//! the upstream default.
//!
//! ## Format
//!
//! The marker `crap-cms:source <version>` is wrapped in the file's native
//! comment syntax (so it doesn't disturb the renderer / parser):
//!
//! - `.hbs`  → `{{!-- crap-cms:source 0.1.0-alpha.8 --}}`
//! - `.js`   → `// crap-cms:source 0.1.0-alpha.8`
//! - `.css`  → `/* crap-cms:source 0.1.0-alpha.8 */`
//! - `.lua`  → `-- crap-cms:source 0.1.0-alpha.8`
//!
//! The header is **optional**: hand-written files or comment-stripped
//! overlays just report `unknown` in `overlay status`. Users can edit or
//! delete the line freely.
//!
//! ## Parsing
//!
//! [`parse_source_version`] looks at the first ~5 lines of any file and
//! extracts the version after `crap-cms:source `. It is forgiving — works
//! across all comment dialects above.

use std::path::Path;

/// Marker text that identifies the source-version line.
const MARKER: &str = "crap-cms:source ";

/// Build the source-version header line for a file path, including the
/// trailing newline. Returns `None` when the file extension isn't one we
/// know how to comment.
pub fn source_header_for(path: &Path, version: &str) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let (prefix, suffix) = match ext {
        "hbs" => ("{{!--", "--}}"),
        "js" | "ts" => ("//", ""),
        "css" => ("/*", "*/"),
        "lua" => ("--", ""),
        _ => return None,
    };

    Some(if suffix.is_empty() {
        format!("{} {}{}\n", prefix, MARKER, version)
    } else {
        format!("{} {}{} {}\n", prefix, MARKER, version, suffix)
    })
}

/// Prepend a source-version header to the file content, if the extension
/// supports one. Returns the original content unchanged when the file
/// type isn't headerable.
pub fn prepend_source_header(path: &Path, version: &str, content: &[u8]) -> Vec<u8> {
    let Some(header) = source_header_for(path, version) else {
        return content.to_vec();
    };

    let mut out = Vec::with_capacity(header.len() + content.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(content);
    out
}

/// Find the `crap-cms:source <version>` marker in the first few lines of
/// a file's content. Returns the version string when present.
pub fn parse_source_version(content: &str) -> Option<String> {
    for line in content.lines().take(5) {
        if let Some(start) = line.find(MARKER) {
            let after = &line[start + MARKER.len()..];
            // Trim trailing comment closers / whitespace.
            let v = after
                .trim()
                .trim_end_matches("--}}")
                .trim_end_matches("*/")
                .trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_for_hbs() {
        let h = source_header_for(Path::new("layout/base.hbs"), "0.1.0").unwrap();
        assert_eq!(h, "{{!-- crap-cms:source 0.1.0 --}}\n");
    }

    #[test]
    fn header_for_js() {
        let h = source_header_for(Path::new("components/toast.js"), "1.2.3").unwrap();
        assert_eq!(h, "// crap-cms:source 1.2.3\n");
    }

    #[test]
    fn header_for_css() {
        let h = source_header_for(Path::new("styles.css"), "0.5.0").unwrap();
        assert_eq!(h, "/* crap-cms:source 0.5.0 */\n");
    }

    #[test]
    fn header_for_lua() {
        let h = source_header_for(Path::new("hook.lua"), "2.0.0").unwrap();
        assert_eq!(h, "-- crap-cms:source 2.0.0\n");
    }

    #[test]
    fn header_skipped_for_unknown_extension() {
        assert!(source_header_for(Path::new("README.md"), "0.1.0").is_none());
    }

    #[test]
    fn header_skipped_for_no_extension() {
        assert!(source_header_for(Path::new("Makefile"), "0.1.0").is_none());
    }

    #[test]
    fn prepend_includes_header_when_supported() {
        let body = b"<h1>Hello</h1>";
        let out = prepend_source_header(Path::new("page.hbs"), "0.1.0", body);
        assert!(String::from_utf8_lossy(&out).starts_with("{{!-- crap-cms:source 0.1.0 --}}\n"));
        assert!(out.ends_with(body));
    }

    #[test]
    fn prepend_passthrough_for_unknown_extension() {
        let body = b"# README";
        let out = prepend_source_header(Path::new("README.md"), "0.1.0", body);
        assert_eq!(out, body);
    }

    #[test]
    fn parse_finds_version_in_hbs_header() {
        let content = "{{!-- crap-cms:source 0.1.0-alpha.8 --}}\n<h1>Hi</h1>";
        assert_eq!(
            parse_source_version(content).as_deref(),
            Some("0.1.0-alpha.8")
        );
    }

    #[test]
    fn parse_finds_version_in_js_header() {
        let content = "// crap-cms:source 1.2.3\nexport const x = 1;";
        assert_eq!(parse_source_version(content).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn parse_finds_version_in_css_header() {
        let content = "/* crap-cms:source 0.5.0 */\n:root { --x: 1; }";
        assert_eq!(parse_source_version(content).as_deref(), Some("0.5.0"));
    }

    #[test]
    fn parse_returns_none_for_no_header() {
        assert!(parse_source_version("<h1>plain template</h1>").is_none());
    }

    #[test]
    fn parse_only_looks_at_first_five_lines() {
        let mut content = String::new();
        for _ in 0..6 {
            content.push_str("filler\n");
        }
        content.push_str("// crap-cms:source 1.0.0\n");
        assert!(parse_source_version(&content).is_none());
    }
}
