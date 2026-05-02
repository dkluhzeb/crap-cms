//! `make page` — scaffold a custom admin page.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

#[cfg(not(tarpaulin_include))]
pub fn run_page(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Page {
        slug,
        label,
        section,
        icon,
        access,
        force,
    } = action
    else {
        unreachable!()
    };

    let slug = match slug {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Page slug (URL becomes /admin/p/<slug>)")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_template_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read page slug")?,
    };

    scaffold::make_page(&scaffold::MakePageOptions {
        config_dir,
        slug: &slug,
        label: label.as_deref(),
        section: section.as_deref(),
        icon: icon.as_deref(),
        access: access.as_deref(),
        force,
    })
}
