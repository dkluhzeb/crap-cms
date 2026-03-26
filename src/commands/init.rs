//! `init` command — scaffold a new config directory with interactive survey.

use anyhow::{Context as _, Result, bail};
use dialoguer::{Confirm, Input};
use nanoid::nanoid;
use std::path::PathBuf;

use super::{load_config_and_sync, make::make_collection_command, user::user_create};
use crate::{
    cli::{self, crap_theme},
    config::CrapConfig,
    scaffold,
};

/// Handle the `init` subcommand — scaffold directory, then optionally create collections
/// and a first admin user via interactive survey.
#[cfg(not(tarpaulin_include))]
pub fn run(dir: Option<PathBuf>, no_input: bool) -> Result<()> {
    let config_dir = match dir {
        Some(d) => d,
        None if no_input => bail!("Directory path is required with --no-input"),
        None => {
            let path: String = Input::with_theme(&crap_theme())
                .with_prompt("Project path")
                .default("./crap-cms".to_string())
                .interact_text()
                .context("Failed to read project path")?;
            PathBuf::from(path)
        }
    };

    if no_input {
        // Non-interactive: scaffold with defaults, create auth + upload collections
        let opts = scaffold::InitOptions::default();
        scaffold::init(Some(config_dir.clone()), &opts)?;

        let auth_opts = scaffold::CollectionOptions {
            auth: true,
            ..scaffold::CollectionOptions::default()
        };
        scaffold::make_collection(&config_dir, "users", None, &auth_opts)?;

        let upload_opts = scaffold::CollectionOptions {
            upload: true,
            ..scaffold::CollectionOptions::default()
        };
        scaffold::make_collection(&config_dir, "media", None, &upload_opts)?;

        println!();
        cli::success("Project created!");
        cli::hint(&format!(
            "Start the server: crap-cms serve {}",
            config_dir.display()
        ));
        return Ok(());
    }

    // --- Interactive mode ---

    cli::info("Welcome to Crap CMS!");

    // Phase 1: Server configuration
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

    // Phase 2: Localization
    cli::step(2, 5, "Localization");

    let enable_locale = Confirm::with_theme(&crap_theme())
        .with_prompt("Enable localization?")
        .default(false)
        .interact()
        .context("Failed to read localization preference")?;

    let (default_locale, locales) = if enable_locale {
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
        (default, all_locales)
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

    // 1. Scaffold the base directory
    scaffold::init(Some(config_dir.clone()), &opts)?;

    // Phase 3: Auth collection
    cli::step(3, 5, "Auth Collection");

    let auth_slug = if Confirm::with_theme(&crap_theme())
        .with_prompt("Create an auth collection (users with login)?")
        .default(true)
        .interact()
        .context("Failed to read auth preference")?
    {
        let slug: String = Input::with_theme(&crap_theme())
            .with_prompt("Auth collection slug")
            .default("users".to_string())
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read auth slug")?;

        // Prompt for custom fields (email/password are always included automatically)
        let fields = if Confirm::with_theme(&crap_theme())
            .with_prompt("Add custom fields? (email/password are included automatically)")
            .default(false)
            .interact()
            .context("Failed to read custom fields preference")?
        {
            scaffold::interactive_field_wizard(enable_locale)?
        } else {
            vec![]
        };

        let opts = scaffold::CollectionOptions {
            auth: true,
            ..scaffold::CollectionOptions::default()
        };

        let fields_opt = if fields.is_empty() {
            None
        } else {
            Some(fields)
        };

        scaffold::make_collection(&config_dir, &slug, fields_opt.as_deref(), &opts)?;

        Some(slug)
    } else {
        None
    };

    // 3. First admin user (right after auth collection)
    if let Some(ref auth_collection) = auth_slug {
        println!();

        if Confirm::with_theme(&crap_theme())
            .with_prompt("Create first admin user now?")
            .default(true)
            .interact()
            .context("Failed to read user creation preference")?
        {
            let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
            let (pool, registry) = load_config_and_sync(&config_dir)?;
            match user_create(
                &pool,
                &registry,
                auth_collection,
                None,
                None,
                vec![],
                &cfg.auth.password_policy,
            ) {
                Ok(()) => {}
                Err(e) => {
                    cli::warning(&format!("Could not create user: {e}"));
                    cli::hint(&format!(
                        "You can create a user later with: crap-cms user create {}",
                        config_dir.display()
                    ));
                }
            }
        } else {
            cli::hint(&format!(
                "You can create a user later with: crap-cms user create {}",
                config_dir.display()
            ));
        }
    }

    // Phase 4: Upload collection
    cli::step(4, 5, "Upload Collection");

    if Confirm::with_theme(&crap_theme())
        .with_prompt("Create an upload collection (file/image uploads)?")
        .default(true)
        .interact()
        .context("Failed to read upload preference")?
    {
        let slug: String = Input::with_theme(&crap_theme())
            .with_prompt("Upload collection slug")
            .default("media".to_string())
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read upload slug")?;

        // Prompt for custom fields (filename/mime_type/size are included automatically)
        let fields = if Confirm::with_theme(&crap_theme())
            .with_prompt("Add custom fields? (filename/mime_type/size are included automatically)")
            .default(false)
            .interact()
            .context("Failed to read custom fields preference")?
        {
            scaffold::interactive_field_wizard(enable_locale)?
        } else {
            vec![]
        };

        let opts = scaffold::CollectionOptions {
            upload: true,
            ..scaffold::CollectionOptions::default()
        };

        let fields_opt = if fields.is_empty() {
            None
        } else {
            Some(fields)
        };

        scaffold::make_collection(&config_dir, &slug, fields_opt.as_deref(), &opts)?;
    }

    // Phase 5: Additional collections
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
            &config_dir,
            None,
            None,
            true, /* interactive */
            &scaffold::CollectionOptions::default(),
        )?;
    }

    println!();
    cli::success("Project created!");
    cli::hint(&format!(
        "Start the server: crap-cms serve {}",
        config_dir.display()
    ));

    Ok(())
}
