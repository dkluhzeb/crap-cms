//! `crap-cms templates list` — list embedded default templates and static files.

use anyhow::Result;

use crate::scaffold;

/// Handle the `templates list` subcommand (no config needed — lists
/// embedded defaults shipped with the binary).
///
/// # Errors
///
/// Returns an error if `r#type` is not one of the accepted values.
pub fn list(r#type: Option<&str>, verbose: bool) -> Result<()> {
    scaffold::templates_list(r#type, verbose)
}
