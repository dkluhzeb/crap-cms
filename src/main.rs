//! CLI entrypoint for Crap CMS. Parses flags, loads config, and starts the admin + gRPC servers.

use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn, error};

mod config;
mod core;
mod db;
mod hooks;
mod admin;
mod api;
mod typegen;

/// Parse a key=value pair for --field arguments.
fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s.find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

#[derive(Parser)]
#[command(name = "crap-cms", about = "Crap CMS - Headless CMS with Lua hooks")]
struct Cli {
    /// Path to the config directory
    #[arg(long, default_value = "./crap-cms")]
    config: PathBuf,

    /// Generate Lua type definitions and exit
    #[arg(long)]
    generate_types: bool,

    /// Create a user in an auth collection and exit
    #[arg(long)]
    create_user: bool,

    /// Auth collection slug (for --create-user)
    #[arg(long, default_value = "users")]
    collection: String,

    /// User email (for --create-user)
    #[arg(long)]
    email: Option<String>,

    /// User password (for --create-user; omit for interactive prompt)
    #[arg(long)]
    password: Option<String>,

    /// Extra fields as key=value pairs (repeatable, for --create-user)
    #[arg(long = "field", value_parser = parse_key_val)]
    fields: Vec<(String, String)>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("crap_cms=debug,info")),
        )
        .init();

    let cli = Cli::parse();
    let config_dir = cli.config.canonicalize().unwrap_or_else(|_| cli.config.clone());

    info!("Config directory: {}", config_dir.display());

    // Load config
    let config = config::CrapConfig::load(&config_dir)
        .context("Failed to load config")?;
    info!(?config, "Configuration loaded");

    // Initialize Lua VM and load collections/globals
    let registry = hooks::init_lua(&config_dir, &config)
        .context("Failed to initialize Lua VM")?;

    {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        info!("Loaded {} collection(s), {} global(s)",
            reg.collections.len(), reg.globals.len());
        for (slug, col) in &reg.collections {
            info!("  Collection '{}': {} field(s)", slug, col.fields.len());
        }
    }

    // Generate Lua type definitions
    if cli.generate_types {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        let path = typegen::generate(&config_dir, &reg)
            .context("Failed to generate type definitions")?;
        println!("{}", path.display());
        return Ok(());
    }

    {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        match typegen::generate(&config_dir, &reg) {
            Ok(path) => info!("Generated type definitions: {}", path.display()),
            Err(e) => warn!("Failed to generate type definitions: {}", e),
        }
    }

    // Initialize database
    let pool = db::pool::create_pool(&config_dir, &config)
        .context("Failed to create database pool")?;

    // Sync database schema from Lua definitions
    db::migrate::sync_all(&pool, &registry)
        .context("Failed to sync database schema")?;

    // Handle --create-user
    if cli.create_user {
        return create_user_command(&cli, &pool, &registry);
    }

    // Initialize Lua hook runner (with registry for CRUD access in hooks)
    let hook_runner = hooks::lifecycle::HookRunner::new(&config_dir, registry.clone(), &config)?;

    // Run on_init hooks (synchronous — failure aborts startup)
    if !config.hooks.on_init.is_empty() {
        info!("Running on_init hooks...");
        let conn = pool.get().context("DB connection for on_init")?;
        hook_runner.run_system_hooks_with_conn(&config.hooks.on_init, &conn)
            .context("on_init hooks failed")?;
        info!("on_init hooks completed");
    }

    // Resolve JWT secret
    let jwt_secret = if config.auth.secret.is_empty() {
        let secret = nanoid::nanoid!(64);
        warn!("No auth.secret in crap.toml — generated random JWT secret (tokens won't survive restarts)");
        secret
    } else {
        config.auth.secret.clone()
    };

    // Log auth collection info
    {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        let auth_collections: Vec<_> = reg.collections.values()
            .filter(|d| d.is_auth_collection())
            .map(|d| d.slug.as_str())
            .collect();
        if auth_collections.is_empty() {
            info!("No auth collections — admin UI and API are open");
        } else {
            info!("Auth collections: {:?} — admin login required", auth_collections);
        }
    }

    // Start servers
    let admin_addr = format!("{}:{}", config.server.host, config.server.admin_port);
    let grpc_addr = format!("{}:{}", config.server.host, config.server.grpc_port);

    info!("Starting Admin UI on http://{}", admin_addr);
    info!("Starting gRPC API on {}", grpc_addr);

    let admin_handle = admin::server::start(
        &admin_addr,
        config.clone(),
        config_dir.clone(),
        pool.clone(),
        registry.clone(),
        hook_runner.clone(),
        jwt_secret.clone(),
    );

    let grpc_handle = api::start_server(
        &grpc_addr,
        pool.clone(),
        registry.clone(),
        hook_runner.clone(),
        jwt_secret,
        &config.depth,
    );

    tokio::try_join!(admin_handle, grpc_handle)
        .map_err(|e| {
            error!("Server error: {}", e);
            e
        })?;

    Ok(())
}

/// Handle the --create-user CLI command.
fn create_user_command(
    cli: &Cli,
    pool: &db::DbPool,
    registry: &core::SharedRegistry,
) -> Result<()> {
    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg.get_collection(&cli.collection)
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", cli.collection))?;

    if !def.is_auth_collection() {
        anyhow::bail!("Collection '{}' is not an auth collection (auth must be enabled)", cli.collection);
    }

    let def = def.clone();
    drop(reg);

    // Get email — from flag or interactive prompt
    let email = match &cli.email {
        Some(e) => e.clone(),
        None => {
            eprint!("Email: ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)
                .context("Failed to read email")?;
            let trimmed = input.trim().to_string();
            if trimmed.is_empty() {
                anyhow::bail!("Email cannot be empty");
            }
            trimmed
        }
    };

    // Get password — from flag or interactive prompt
    let password = match &cli.password {
        Some(p) => {
            warn!("Password provided via command line — it may be visible in shell history");
            p.clone()
        }
        None => {
            eprint!("Password: ");
            let p1 = rpassword::read_password()
                .context("Failed to read password")?;
            if p1.is_empty() {
                anyhow::bail!("Password cannot be empty");
            }
            eprint!("Confirm password: ");
            let p2 = rpassword::read_password()
                .context("Failed to read password confirmation")?;
            if p1 != p2 {
                anyhow::bail!("Passwords do not match");
            }
            p1
        }
    };

    // Build data map from email + extra --field args
    let mut data: HashMap<String, String> = cli.fields.iter().cloned().collect();
    data.insert("email".to_string(), email);

    // Prompt for any required fields not already provided
    for field in &def.fields {
        if field.name == "email" {
            continue; // already handled above
        }
        if field.field_type == core::field::FieldType::Checkbox {
            continue; // absent checkbox = false, always valid
        }
        if data.contains_key(&field.name) {
            continue; // already provided via --field
        }
        if !field.required && field.default_value.is_none() {
            continue; // optional with no default — skip
        }
        // Use default_value if available and field is not required
        if !field.required {
            if let Some(ref dv) = field.default_value {
                let val = match dv {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                data.insert(field.name.clone(), val);
                continue;
            }
        }
        // Required field with a default — use it automatically
        if let Some(ref dv) = field.default_value {
            let val = match dv {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            eprint!("{} [{}]: ", field.name, val);
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)
                .with_context(|| format!("Failed to read {}", field.name))?;
            let trimmed = input.trim();
            if trimmed.is_empty() {
                data.insert(field.name.clone(), val);
            } else {
                data.insert(field.name.clone(), trimmed.to_string());
            }
            continue;
        }
        // Required field, no default — must prompt
        eprint!("{}: ", field.name);
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)
            .with_context(|| format!("Failed to read {}", field.name))?;
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("{} is required", field.name);
        }
        data.insert(field.name.clone(), trimmed);
    }

    // Create user in a transaction
    let mut conn = pool.get().context("Failed to get database connection")?;
    let tx = conn.transaction().context("Failed to begin transaction")?;

    let doc = db::query::create(&tx, &cli.collection, &def, &data)
        .context("Failed to create user")?;

    db::query::update_password(&tx, &cli.collection, &doc.id, &password)
        .context("Failed to set password")?;

    tx.commit().context("Failed to commit transaction")?;

    println!("Created user {} in '{}'", doc.id, cli.collection);

    Ok(())
}
