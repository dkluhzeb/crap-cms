//! `make route` — scaffold a custom HTTP route handler.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

/// Handle the `make route` subcommand — resolve the handler name interactively
/// if missing, then scaffold the file and print the registration snippet.
#[cfg(not(tarpaulin_include))]
pub(super) fn run_route(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Route {
        name,
        method,
        path,
        force,
    } = action
    else {
        unreachable!()
    };

    let name = match name {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Route handler name")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read route name")?,
    };

    scaffold::make_route(&scaffold::MakeRouteOptions {
        config_dir,
        name: &name,
        method: method.as_deref(),
        path: path.as_deref(),
        force,
    })
}
