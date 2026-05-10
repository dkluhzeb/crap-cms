//! `crap-cms templates extract` — write embedded defaults into the config dir
//! with source-version headers so later runs can detect drift.

use std::path::Path;

use anyhow::Result;

use crate::scaffold;

/// Handle the `templates extract` subcommand (writes embedded defaults
/// into the config dir, with source-version headers).
pub fn extract(
    config_dir: &Path,
    paths: &[String],
    all: bool,
    r#type: Option<String>,
    force: bool,
) -> Result<()> {
    scaffold::templates_extract(config_dir, paths, all, r#type.as_deref(), force)
}
