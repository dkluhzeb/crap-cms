//! `init` command — scaffold a new config directory with interactive survey.

use anyhow::{Context as _, Result};
use std::path::PathBuf;

/// Handle the `init` subcommand — scaffold directory, then optionally create collections
/// and a first admin user via interactive survey.
#[cfg(not(tarpaulin_include))]
pub fn run(dir: Option<PathBuf>) -> Result<()> {
    use dialoguer::{Confirm, Input};

    let config_dir = dir.clone().unwrap_or_else(|| PathBuf::from("./crap-cms"));

    // Collect init options via interactive prompts
    let admin_port: u16 = Input::new()
        .with_prompt("Admin port")
        .default(3000)
        .interact_text()
        .context("Failed to read admin port")?;

    let grpc_port: u16 = Input::new()
        .with_prompt("gRPC port")
        .default(50051)
        .interact_text()
        .context("Failed to read gRPC port")?;

    let enable_locale = Confirm::new()
        .with_prompt("Enable localization?")
        .default(false)
        .interact()
        .context("Failed to read localization preference")?;

    let (default_locale, locales) = if enable_locale {
        let default: String = Input::new()
            .with_prompt("Default locale")
            .default("en".to_string())
            .interact_text()
            .context("Failed to read default locale")?;

        let extra: String = Input::new()
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

    let opts = crate::scaffold::InitOptions {
        admin_port,
        grpc_port,
        locales,
        default_locale,
        auth_secret: nanoid::nanoid!(64),
    };

    // 1. Scaffold the base directory
    crate::scaffold::init(dir, &opts)?;

    println!();

    // 2. Auth collection
    let auth_slug = if Confirm::new()
        .with_prompt("Create an auth collection (users with login)?")
        .default(true)
        .interact()
        .context("Failed to read auth preference")?
    {
        let slug: String = Input::new()
            .with_prompt("Auth collection slug")
            .default("users".to_string())
            .validate_with(|input: &String| -> Result<(), String> {
                crate::scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read auth slug")?;
        crate::scaffold::make_collection(&config_dir, &slug, None, false, true, false, false, false)?;
        Some(slug)
    } else {
        None
    };

    // 3. First admin user (right after auth collection)
    if let Some(ref auth_collection) = auth_slug {
        println!();
        if Confirm::new()
            .with_prompt("Create first admin user now?")
            .default(true)
            .interact()
            .context("Failed to read user creation preference")?
        {
            let cfg = crate::config::CrapConfig::load(&config_dir)
                .context("Failed to load config")?;
            let (pool, registry) = super::load_config_and_sync(&config_dir)?;
            super::user::user_create(&pool, &registry, auth_collection, None, None, vec![], &cfg.auth.password_policy)?;
        } else {
            println!("You can create a user later with:");
            println!("  crap-cms user create {}", config_dir.display());
        }
        println!();
    }

    // 4. Upload collection
    if Confirm::new()
        .with_prompt("Create an upload collection (file/image uploads)?")
        .default(true)
        .interact()
        .context("Failed to read upload preference")?
    {
        let slug: String = Input::new()
            .with_prompt("Upload collection slug")
            .default("media".to_string())
            .validate_with(|input: &String| -> Result<(), String> {
                crate::scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read upload slug")?;
        crate::scaffold::make_collection(&config_dir, &slug, None, false, false, true, false, false)?;
    }

    // 5. Additional collections
    loop {
        println!();
        if !Confirm::new()
            .with_prompt("Create another collection?")
            .default(false)
            .interact()
            .context("Failed to read collection preference")?
        {
            break;
        }
        super::make::make_collection_command(&config_dir, None, None, false, false, false, false, true /* interactive */, false)?;
    }

    println!();
    println!("Start the server: crap-cms serve {}", config_dir.display());

    Ok(())
}
