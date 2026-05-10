//! `crap-cms templates list` — list embedded default templates and static files.

use anyhow::Result;

use crate::scaffold;

/// Handle the `templates list` subcommand (no config needed — lists
/// embedded defaults shipped with the binary).
pub fn list(r#type: Option<String>, verbose: bool) -> Result<()> {
    scaffold::templates_list(r#type.as_deref(), verbose)
}
