//! `crap-cms templates extract` — write embedded defaults into the config dir
//! with source-version headers so later runs can detect drift.

use std::path::Path;

use anyhow::Result;

use crate::scaffold;

/// Handle the `templates extract` subcommand (writes embedded defaults
/// into the config dir, with source-version headers).
///
/// # Errors
///
/// Returns an error if no paths are given without `--all`, an unknown path
/// is requested, or any file write fails.
pub fn extract(
    config_dir: &Path,
    paths: &[String],
    all: bool,
    r#type: Option<&str>,
    force: bool,
) -> Result<()> {
    scaffold::templates_extract(&scaffold::TemplatesExtractParams {
        config_dir,
        paths,
        all,
        type_filter: r#type,
        force,
    })
}
