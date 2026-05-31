//! Shared helpers used by multiple `crap-cms templates` subcommands.

use include_dir::Dir;

use crate::scaffold::{EMBEDDED_STATIC, EMBEDDED_TEMPLATES};

/// Current crate version — what an overlay file's source-version header
/// is compared against.
pub(super) const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Split a relative overlay path like `templates/layout/base.hbs` or
/// `static/styles.css` into the overlay-root kind and the sub-path.
/// Returns `None` for paths outside the two known overlay roots.
pub(super) fn split_kind(rel_path: &str) -> Option<(&'static str, &str)> {
    if let Some(rest) = rel_path.strip_prefix("templates/") {
        Some(("templates", rest))
    } else if let Some(rest) = rel_path.strip_prefix("static/") {
        Some(("static", rest))
    } else {
        None
    }
}

/// Return the embedded upstream bytes for `kind/sub_path`, or `None` if
/// no compiled-in default exists at that path.
pub(super) fn lookup_embedded(kind: &str, sub_path: &str) -> Option<&'static [u8]> {
    let dir: &'static Dir = match kind {
        "templates" => &EMBEDDED_TEMPLATES,
        "static" => &EMBEDDED_STATIC,
        _ => return None,
    };

    dir.get_file(sub_path).map(include_dir::File::contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_kind_recognizes_both_overlay_roots() {
        assert_eq!(
            split_kind("templates/layout/base.hbs"),
            Some(("templates", "layout/base.hbs"))
        );
        assert_eq!(
            split_kind("static/styles.css"),
            Some(("static", "styles.css"))
        );
    }

    #[test]
    fn split_kind_rejects_paths_outside_known_roots() {
        assert_eq!(split_kind("config/crap.toml"), None);
        assert_eq!(split_kind("templates"), None); // prefix is "templates/", bare dir has no slash
        assert_eq!(split_kind(""), None);
    }

    #[test]
    fn lookup_embedded_returns_none_for_unknown_kind() {
        assert!(lookup_embedded("collections", "anything").is_none());
    }

    #[test]
    fn lookup_embedded_returns_none_for_missing_file() {
        assert!(lookup_embedded("templates", "does/not/exist.hbs").is_none());
        assert!(lookup_embedded("static", "no-such-file.css").is_none());
    }

    #[test]
    fn lookup_embedded_finds_a_known_compiled_in_template() {
        // base.hbs is the root layout — guaranteed compiled in.
        let bytes = lookup_embedded("templates", "layout/base.hbs");
        assert!(bytes.is_some_and(|b| !b.is_empty()));
    }
}
