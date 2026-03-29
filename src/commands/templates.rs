//! `templates` command — list and extract default admin templates and static files.

use anyhow::Result;
use std::path::Path;

use crate::scaffold;

/// Handle the `templates list` subcommand (no config needed).
pub fn list(r#type: Option<String>, verbose: bool) -> Result<()> {
    scaffold::templates_list(r#type.as_deref(), verbose)
}

/// Handle the `templates extract` subcommand (needs config dir).
pub fn extract(
    config_dir: &Path,
    paths: &[String],
    all: bool,
    r#type: Option<String>,
    force: bool,
) -> Result<()> {
    scaffold::templates_extract(config_dir, paths, all, r#type.as_deref(), force)
}
