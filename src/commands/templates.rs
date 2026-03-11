//! `templates` command — list and extract default admin templates and static files.

use anyhow::Result;

/// Handle the `templates` subcommand.
pub fn run(action: super::TemplatesAction) -> Result<()> {
    match action {
        super::TemplatesAction::List { r#type, verbose } => {
            crate::scaffold::templates_list(r#type.as_deref(), verbose)
        }
        super::TemplatesAction::Extract {
            config,
            paths,
            all,
            r#type,
            force,
        } => crate::scaffold::templates_extract(&config, &paths, all, r#type.as_deref(), force),
    }
}
