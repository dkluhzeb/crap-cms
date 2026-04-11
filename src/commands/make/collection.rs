//! `make collection` — scaffold a new collection with interactive survey.

use anyhow::{Context as _, Result, bail};
use dialoguer::{Confirm, Input};
use std::path::Path;

use crate::{
    cli::crap_theme,
    commands::MakeAction,
    scaffold::{self, CollectionOptions},
};

use super::helpers::has_locales_enabled;

/// Entry point from the `run` dispatcher — destructures CLI args and delegates.
#[cfg(not(tarpaulin_include))]
pub fn run_collection(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Collection {
        slug,
        fields,
        no_timestamps,
        auth,
        upload,
        versions,
        no_input,
        force,
    } = action
    else {
        unreachable!()
    };

    let opts = CollectionOptions {
        no_timestamps,
        auth,
        upload,
        versions,
        force,
    };

    make_collection_command(config_dir, slug, fields, !no_input, &opts)
}

/// Handle the `make collection` subcommand — resolve missing args via interactive survey.
#[cfg(not(tarpaulin_include))]
pub(crate) fn make_collection_command(
    config_dir: &Path,
    slug: Option<String>,
    fields: Option<String>,
    interactive: bool,
    opts: &CollectionOptions,
) -> Result<()> {
    let slug = resolve_slug(slug, interactive)?;
    let (auth, upload) = resolve_type_flags(opts, interactive)?;
    let parsed_fields = resolve_fields(config_dir, fields, interactive, auth, upload)?;
    let no_timestamps = resolve_timestamps(opts, interactive)?;
    let versions = resolve_versions(opts, interactive)?;

    let final_opts = CollectionOptions {
        no_timestamps,
        auth,
        upload,
        versions,
        force: opts.force,
    };

    scaffold::make_collection(config_dir, &slug, parsed_fields.as_deref(), &final_opts)
}

/// Resolve collection slug from CLI arg or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_slug(slug: Option<String>, interactive: bool) -> Result<String> {
    match slug {
        Some(s) => Ok(s),
        None if interactive => Input::with_theme(&crap_theme())
            .with_prompt("Collection slug")
            .validate_with(|input: &String| -> Result<(), String> {
                if input.is_empty() {
                    return Err("Slug cannot be empty".into());
                }

                if !input
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    return Err("Use lowercase letters, digits, and underscores only".into());
                }

                if input.starts_with('_') {
                    return Err("Slug cannot start with underscore".into());
                }

                Ok(())
            })
            .interact_text()
            .context("Failed to read collection slug"),
        None => {
            bail!("Collection slug is required (or omit --no-input for interactive mode)")
        }
    }
}

/// Resolve auth and upload flags from CLI opts or interactive prompts.
#[cfg(not(tarpaulin_include))]
fn resolve_type_flags(opts: &CollectionOptions, interactive: bool) -> Result<(bool, bool)> {
    let auth = if opts.auth {
        true
    } else if interactive {
        Confirm::with_theme(&crap_theme())
            .with_prompt("Auth collection (email/password login)?")
            .default(false)
            .interact()
            .context("Failed to read auth preference")?
    } else {
        false
    };

    let upload = if opts.upload {
        true
    } else if interactive {
        Confirm::with_theme(&crap_theme())
            .with_prompt("Upload collection (file uploads)?")
            .default(false)
            .interact()
            .context("Failed to read upload preference")?
    } else {
        false
    };

    Ok((auth, upload))
}

/// Resolve fields from CLI shorthand or interactive wizard.
#[cfg(not(tarpaulin_include))]
fn resolve_fields(
    config_dir: &Path,
    fields: Option<String>,
    interactive: bool,
    auth: bool,
    upload: bool,
) -> Result<Option<Vec<scaffold::FieldStub>>> {
    match fields {
        Some(s) => Ok(Some(scaffold::parse_fields_shorthand(&s)?)),
        None if interactive && (auth || upload) => {
            let hint = if auth {
                "email/password are included automatically"
            } else {
                "filename/mime_type/size are included automatically"
            };

            if Confirm::with_theme(&crap_theme())
                .with_prompt(format!("Add custom fields? ({})", hint))
                .default(false)
                .interact()
                .context("Failed to read custom fields preference")?
            {
                let f = scaffold::interactive_field_wizard(has_locales_enabled(config_dir))?;
                Ok(if f.is_empty() { None } else { Some(f) })
            } else {
                Ok(None)
            }
        }
        None if interactive => {
            let f = scaffold::interactive_field_wizard(has_locales_enabled(config_dir))?;
            Ok(if f.is_empty() { None } else { Some(f) })
        }
        None => Ok(None),
    }
}

/// Resolve timestamps flag from CLI opts or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_timestamps(opts: &CollectionOptions, interactive: bool) -> Result<bool> {
    if opts.no_timestamps {
        return Ok(true);
    }

    if interactive {
        let timestamps = Confirm::with_theme(&crap_theme())
            .with_prompt("Enable timestamps?")
            .default(true)
            .interact()
            .context("Failed to read timestamps preference")?;

        return Ok(!timestamps);
    }

    Ok(false)
}

/// Resolve versioning flag from CLI opts or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_versions(opts: &CollectionOptions, interactive: bool) -> Result<bool> {
    if opts.versions {
        return Ok(true);
    }

    if interactive {
        return Confirm::with_theme(&crap_theme())
            .with_prompt("Enable versioning (draft/publish workflow)?")
            .default(false)
            .interact()
            .context("Failed to read versioning preference");
    }

    Ok(false)
}
