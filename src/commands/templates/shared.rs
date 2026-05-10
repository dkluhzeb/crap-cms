//! Shared helpers used by multiple `crap-cms templates` subcommands.

use include_dir::Dir;

use crate::scaffold::templates::{EMBEDDED_STATIC, EMBEDDED_TEMPLATES};

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

    dir.get_file(sub_path).map(|f| f.contents())
}
