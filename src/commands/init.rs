//! `init` command — scaffold a new config directory with interactive survey.

use anyhow::{Context as _, Result, bail};
use dialoguer::{Confirm, Input};
use nanoid::nanoid;
use std::path::{Path, PathBuf};

use crate::{
    cli::{self, crap_theme},
    commands::{load_config_and_sync, make::make_collection_command, user::user_create},
    config::CrapConfig,
    scaffold,
};

/// Handle the `init` subcommand — scaffold directory, then optionally create collections
/// and a first admin user via interactive survey.
#[cfg(not(tarpaulin_include))]
pub fn run(dir: Option<PathBuf>, no_input: bool) -> Result<()> {
    let config_dir = resolve_directory(dir, no_input)?;

    if no_input {
        return run_non_interactive(&config_dir);
    }

    run_interactive(&config_dir)
}

/// Resolve the target directory from CLI arg or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_directory(dir: Option<PathBuf>, no_input: bool) -> Result<PathBuf> {
    match dir {
        Some(d) => Ok(d),
        None if no_input => bail!("Directory path is required with --no-input"),
        None => {
            let path: String = Input::with_theme(&crap_theme())
                .with_prompt("Project path")
                .default("./crap-cms".to_string())
                .interact_text()
                .context("Failed to read project path")?;

            Ok(PathBuf::from(path))
        }
    }
}

/// Non-interactive init: scaffold with defaults, create auth + upload collections.
#[cfg(not(tarpaulin_include))]
fn run_non_interactive(config_dir: &Path) -> Result<()> {
    let opts = scaffold::InitOptions::default();

    scaffold::init(Some(config_dir.to_path_buf()), &opts)?;

    scaffold::make_collection(
        config_dir,
        "users",
        None,
        &scaffold::CollectionOptions {
            auth: true,
            ..Default::default()
        },
    )?;

    scaffold::make_collection(
        config_dir,
        "media",
        None,
        &scaffold::CollectionOptions {
            upload: true,
            ..Default::default()
        },
    )?;

    println!();
    cli::success("Project created!");
    cli::hint(&format!(
        "Start the server: crap-cms serve {}",
        config_dir.display()
    ));

    Ok(())
}

/// Interactive init wizard: 5-phase survey for project setup.
#[cfg(not(tarpaulin_include))]
fn run_interactive(config_dir: &Path) -> Result<()> {
    cli::info("Welcome to Crap CMS!");

    let (opts, enable_locale) = prompt_server_and_locale()?;

    scaffold::init(Some(config_dir.to_path_buf()), &opts)?;

    let auth_slug = prompt_auth_collection(config_dir, enable_locale)?;

    if let Some(ref slug) = auth_slug {
        prompt_first_user(config_dir, slug)?;
    }

    prompt_upload_collection(config_dir, enable_locale)?;
    prompt_additional_collections(config_dir)?;

    println!();
    cli::success("Project created!");
    cli::hint(&format!(
        "Start the server: crap-cms serve {}",
        config_dir.display()
    ));

    Ok(())
}

/// Phase 1+2: Prompt for server ports and locale configuration.
#[cfg(not(tarpaulin_include))]
fn prompt_server_and_locale() -> Result<(scaffold::InitOptions, bool)> {
    cli::step(1, 5, "Server Configuration");

    let admin_port: u16 = Input::with_theme(&crap_theme())
        .with_prompt("Admin port")
        .default(3000)
        .interact_text()
        .context("Failed to read admin port")?;

    let grpc_port: u16 = Input::with_theme(&crap_theme())
        .with_prompt("gRPC port")
        .default(50051)
        .interact_text()
        .context("Failed to read gRPC port")?;

    cli::step(2, 5, "Localization");

    let enable_locale = Confirm::with_theme(&crap_theme())
        .with_prompt("Enable localization?")
        .default(false)
        .interact()
        .context("Failed to read localization preference")?;

    let (default_locale, locales) = if enable_locale {
        prompt_locale_settings()?
    } else {
        ("en".to_string(), vec![])
    };

    let opts = scaffold::InitOptions {
        admin_port,
        grpc_port,
        locales,
        default_locale,
        auth_secret: nanoid!(64),
    };

    Ok((opts, enable_locale))
}

/// Prompt for default locale and additional locales.
#[cfg(not(tarpaulin_include))]
fn prompt_locale_settings() -> Result<(String, Vec<String>)> {
    let default: String = Input::with_theme(&crap_theme())
        .with_prompt("Default locale")
        .default("en".to_string())
        .interact_text()
        .context("Failed to read default locale")?;

    let extra: String = Input::with_theme(&crap_theme())
        .with_prompt("Additional locales (comma-separated, e.g. \"de,fr\")")
        .default(String::new())
        .allow_empty(true)
        .interact_text()
        .context("Failed to read additional locales")?;

    let mut all_locales = vec![default.clone()];

    for l in extra.split(',') {
        let l = l.trim().to_string();

        if !l.is_empty() && !all_locales.contains(&l) {
            all_locales.push(l);
        }
    }

    Ok((default, all_locales))
}

/// Phase 3: Prompt to create an auth collection with optional custom fields.
#[cfg(not(tarpaulin_include))]
fn prompt_auth_collection(config_dir: &Path, enable_locale: bool) -> Result<Option<String>> {
    cli::step(3, 5, "Auth Collection");

    if !Confirm::with_theme(&crap_theme())
        .with_prompt("Create an auth collection (users with login)?")
        .default(true)
        .interact()
        .context("Failed to read auth preference")?
    {
        return Ok(None);
    }

    let slug = prompt_collection_slug("Auth collection slug", "users")?;
    let fields = prompt_optional_fields(
        "Add custom fields? (email/password are included automatically)",
        enable_locale,
    )?;

    let fields_opt = if fields.is_empty() {
        None
    } else {
        Some(fields)
    };

    scaffold::make_collection(
        config_dir,
        &slug,
        fields_opt.as_deref(),
        &scaffold::CollectionOptions {
            auth: true,
            ..Default::default()
        },
    )?;

    Ok(Some(slug))
}

/// Prompt to create the first admin user after auth collection setup.
#[cfg(not(tarpaulin_include))]
fn prompt_first_user(config_dir: &Path, auth_collection: &str) -> Result<()> {
    println!();

    if !Confirm::with_theme(&crap_theme())
        .with_prompt("Create first admin user now?")
        .default(true)
        .interact()
        .context("Failed to read user creation preference")?
    {
        cli::hint(&format!(
            "You can create a user later with: crap-cms user create {}",
            config_dir.display()
        ));

        return Ok(());
    }

    let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;
    let (pool, registry) = load_config_and_sync(config_dir)?;

    if let Err(e) = user_create(
        &pool,
        &registry,
        auth_collection,
        None,
        None,
        vec![],
        &cfg.auth.password_policy,
    ) {
        cli::warning(&format!("Could not create user: {e}"));
        cli::hint(&format!(
            "You can create a user later with: crap-cms user create {}",
            config_dir.display()
        ));
    }

    Ok(())
}

/// Phase 4: Prompt to create an upload collection with optional custom fields.
#[cfg(not(tarpaulin_include))]
fn prompt_upload_collection(config_dir: &Path, enable_locale: bool) -> Result<()> {
    cli::step(4, 5, "Upload Collection");

    if !Confirm::with_theme(&crap_theme())
        .with_prompt("Create an upload collection (file/image uploads)?")
        .default(true)
        .interact()
        .context("Failed to read upload preference")?
    {
        return Ok(());
    }

    let slug = prompt_collection_slug("Upload collection slug", "media")?;
    let fields = prompt_optional_fields(
        "Add custom fields? (filename/mime_type/size are included automatically)",
        enable_locale,
    )?;

    let fields_opt = if fields.is_empty() {
        None
    } else {
        Some(fields)
    };

    scaffold::make_collection(
        config_dir,
        &slug,
        fields_opt.as_deref(),
        &scaffold::CollectionOptions {
            upload: true,
            ..Default::default()
        },
    )?;

    Ok(())
}

/// Phase 5: Loop to create additional collections.
#[cfg(not(tarpaulin_include))]
fn prompt_additional_collections(config_dir: &Path) -> Result<()> {
    cli::step(5, 5, "Additional Collections");

    loop {
        if !Confirm::with_theme(&crap_theme())
            .with_prompt("Create another collection?")
            .default(false)
            .interact()
            .context("Failed to read collection preference")?
        {
            break;
        }

        make_collection_command(
            config_dir,
            None,
            None,
            true,
            &scaffold::CollectionOptions::default(),
        )?;
    }

    Ok(())
}

/// Prompt for a collection slug with validation.
#[cfg(not(tarpaulin_include))]
fn prompt_collection_slug(prompt: &str, default: &str) -> Result<String> {
    Input::with_theme(&crap_theme())
        .with_prompt(prompt)
        .default(default.to_string())
        .validate_with(|input: &String| -> Result<(), String> {
            scaffold::validate_slug(input).map_err(|e| e.to_string())
        })
        .interact_text()
        .context("Failed to read collection slug")
}

/// Prompt whether to add custom fields, and if so, run the field wizard.
#[cfg(not(tarpaulin_include))]
fn prompt_optional_fields(prompt: &str, enable_locale: bool) -> Result<Vec<scaffold::FieldStub>> {
    if Confirm::with_theme(&crap_theme())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .context("Failed to read custom fields preference")?
    {
        scaffold::interactive_field_wizard(enable_locale)
    } else {
        Ok(vec![])
    }
}
