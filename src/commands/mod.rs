//! CLI command handlers. Each submodule handles one top-level subcommand.

pub mod db;
pub mod export;
pub mod images;
pub mod init;
pub mod jobs;
pub mod make;
pub mod mcp;
pub mod serve;
pub mod status;
pub mod templates;
pub mod typegen;
pub mod user;

use anyhow::{Context as _, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};

/// Parse a key=value pair for --field arguments.
pub fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

#[derive(Subcommand)]
pub enum MakeAction {
    /// Generate a collection Lua file
    Collection {
        /// Path to the config directory
        config: PathBuf,

        /// Collection slug (e.g., "posts"). Prompted if omitted.
        slug: Option<String>,

        /// Inline field shorthand (e.g., "title:text:required,status:select,body:textarea")
        #[arg(short = 'F', long)]
        fields: Option<String>,

        /// Set timestamps = false
        #[arg(short = 'T', long)]
        no_timestamps: bool,

        /// Enable auth (email/password login)
        #[arg(long)]
        auth: bool,

        /// Enable uploads (file upload collection)
        #[arg(long)]
        upload: bool,

        /// Enable versioning (draft/publish workflow)
        #[arg(long)]
        versions: bool,

        /// Non-interactive mode — skip all prompts, use flags and defaults only
        #[arg(long)]
        no_input: bool,

        /// Overwrite existing file
        #[arg(short, long)]
        force: bool,
    },

    /// Generate a global Lua file
    Global {
        /// Path to the config directory
        config: PathBuf,

        /// Global slug (e.g., "site_settings"). Prompted if omitted.
        slug: Option<String>,

        /// Inline field shorthand (e.g., "title:text:required,tagline:textarea")
        #[arg(short = 'F', long)]
        fields: Option<String>,

        /// Overwrite existing file
        #[arg(short, long)]
        force: bool,
    },

    /// Generate a hook file (file-per-hook pattern)
    Hook {
        /// Path to the config directory
        config: PathBuf,

        /// Hook function name (e.g., "auto_slug"). Prompted if omitted.
        name: Option<String>,

        /// Hook type: collection, field, or access
        #[arg(short = 't', long = "type")]
        hook_type: Option<String>,

        /// Target collection slug
        #[arg(short, long)]
        collection: Option<String>,

        /// Lifecycle position (e.g., before_change, after_read)
        #[arg(short = 'l', long)]
        position: Option<String>,

        /// Target field name (field hooks only)
        #[arg(short = 'F', long)]
        field: Option<String>,

        /// Overwrite existing file
        #[arg(long)]
        force: bool,
    },

    /// Generate a job Lua file
    Job {
        /// Path to the config directory
        config: PathBuf,

        /// Job slug (e.g., "cleanup_expired"). Prompted if omitted.
        slug: Option<String>,

        /// Cron schedule expression (e.g., "0 3 * * *")
        #[arg(short, long)]
        schedule: Option<String>,

        /// Queue name (default: "default")
        #[arg(short, long)]
        queue: Option<String>,

        /// Max retry attempts (default: 0)
        #[arg(short, long)]
        retries: Option<u32>,

        /// Timeout in seconds (default: 60)
        #[arg(short, long)]
        timeout: Option<u64>,

        /// Overwrite existing file
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum BlueprintAction {
    /// Save a config directory as a reusable blueprint
    Save {
        /// Path to the config directory
        config: PathBuf,

        /// Blueprint name (e.g., "blog", "saas-starter")
        name: String,

        /// Overwrite existing blueprint
        #[arg(short, long)]
        force: bool,
    },

    /// Create a new project from a saved blueprint
    Use {
        /// Blueprint name to use. Prompted if omitted.
        name: Option<String>,

        /// Directory to create (default: ./crap-cms)
        dir: Option<PathBuf>,
    },

    /// List all saved blueprints
    List,

    /// Remove a saved blueprint
    Remove {
        /// Blueprint name to remove. Prompted if omitted.
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum UserAction {
    /// Create a new user in an auth collection
    Create {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User password (omit for interactive prompt)
        #[arg(short, long)]
        password: Option<String>,

        /// Extra fields as key=value pairs (repeatable)
        #[arg(short, long = "field", value_parser = parse_key_val)]
        fields: Vec<(String, String)>,
    },

    /// List users in an auth collection
    List {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,
    },

    /// Show detailed info for a single user
    Info {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User ID
        #[arg(long)]
        id: Option<String>,
    },

    /// Delete a user from an auth collection
    Delete {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User ID
        #[arg(long)]
        id: Option<String>,

        /// Skip confirmation prompt
        #[arg(short = 'y', long)]
        confirm: bool,
    },

    /// Lock a user account (prevent login)
    Lock {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User ID
        #[arg(long)]
        id: Option<String>,
    },

    /// Unlock a user account (allow login)
    Unlock {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User ID
        #[arg(long)]
        id: Option<String>,
    },

    /// Verify a user account (mark email as verified)
    Verify {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User ID
        #[arg(long)]
        id: Option<String>,
    },

    /// Unverify a user account (mark email as unverified)
    Unverify {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User ID
        #[arg(long)]
        id: Option<String>,
    },

    /// Change a user's password
    ChangePassword {
        /// Path to the config directory
        config: PathBuf,

        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,

        /// User email
        #[arg(short, long)]
        email: Option<String>,

        /// User ID
        #[arg(long)]
        id: Option<String>,

        /// New password (omit for interactive prompt)
        #[arg(short, long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum MigrateAction {
    /// Create a new migration file
    Create {
        /// Migration name (e.g., "add_categories")
        name: String,
    },
    /// Schema sync + run pending Lua data migrations
    Up,
    /// Rollback last N data migrations
    Down {
        /// Number of migrations to roll back
        #[arg(short, long, default_value = "1")]
        steps: usize,
    },
    /// Show all migration files with applied/pending status
    List,
    /// Drop all tables, recreate from Lua definitions, run all migrations
    Fresh {
        /// Required confirmation flag (destructive operation)
        #[arg(short = 'y', long)]
        confirm: bool,
    },
}

#[derive(Subcommand)]
pub enum DbAction {
    /// Open an interactive SQLite console
    Console {
        /// Path to the config directory
        config: PathBuf,
    },
    /// Detect and optionally remove orphan columns not in Lua definitions
    Cleanup {
        /// Path to the config directory
        config: PathBuf,

        /// Actually drop orphan columns (default: dry-run report only)
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Subcommand)]
pub enum TemplatesAction {
    /// List all available default templates and static files
    List {
        /// Filter: "templates" or "static" (default: both)
        #[arg(short, long)]
        r#type: Option<String>,

        /// Show full file tree with individual file sizes
        #[arg(short, long)]
        verbose: bool,
    },
    /// Extract default files into the config directory for customization
    Extract {
        /// Path to the config directory
        config: PathBuf,
        /// File paths to extract (e.g., "layout/base.hbs" "styles.css")
        paths: Vec<String>,
        /// Extract all files
        #[arg(short, long)]
        all: bool,
        /// Filter: "templates" or "static" (default: both, only with --all)
        #[arg(short, long)]
        r#type: Option<String>,
        /// Overwrite existing files
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum JobsAction {
    /// List defined jobs and recent runs
    List {
        /// Path to the config directory
        config: PathBuf,
    },
    /// Trigger a job manually
    Trigger {
        /// Path to the config directory
        config: PathBuf,
        /// Job slug to trigger
        slug: String,
        /// JSON data to pass to the job (default: "{}")
        #[arg(short, long)]
        data: Option<String>,
    },
    /// Show job run history
    Status {
        /// Path to the config directory
        config: PathBuf,
        /// Show a single job run by ID
        #[arg(long)]
        id: Option<String>,
        /// Filter by job slug
        #[arg(short, long)]
        slug: Option<String>,
        /// Max results to show
        #[arg(short, long, default_value = "20")]
        limit: i64,
    },
    /// Clean up old completed/failed job runs
    Purge {
        /// Path to the config directory
        config: PathBuf,
        /// Delete runs older than this (e.g., "7d", "24h", "30m")
        #[arg(long, default_value = "7d")]
        older_than: String,
    },
    /// Check job system health
    Healthcheck {
        /// Path to the config directory
        config: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum ImagesAction {
    /// List image processing queue entries
    List {
        /// Path to the config directory
        config: PathBuf,

        /// Filter by status: pending, processing, completed, failed
        #[arg(short, long)]
        status: Option<String>,

        /// Max entries to show
        #[arg(short, long, default_value = "20")]
        limit: i64,
    },
    /// Show queue statistics by status
    Stats {
        /// Path to the config directory
        config: PathBuf,
    },
    /// Retry failed queue entries
    Retry {
        /// Path to the config directory
        config: PathBuf,

        /// Retry a specific entry by ID
        #[arg(long)]
        id: Option<String>,

        /// Retry all failed entries
        #[arg(long)]
        all: bool,

        /// Confirm retry all (required with --all)
        #[arg(short = 'y', long)]
        confirm: bool,
    },
    /// Purge old completed/failed entries
    Purge {
        /// Path to the config directory
        config: PathBuf,

        /// Delete entries older than this (e.g., "7d", "24h", "30m")
        #[arg(long, default_value = "7d")]
        older_than: String,
    },
}

/// Load config, init Lua, create pool, and sync schema. Shared by user, export, import commands.
pub fn load_config_and_sync(
    config_dir: &Path,
) -> Result<(crate::db::DbPool, crate::core::SharedRegistry)> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = crate::config::CrapConfig::load(&config_dir).context("Failed to load config")?;

    // Check crap_version compatibility
    if let Some(warning) = cfg.check_version() {
        tracing::warn!("{}", warning);
    }

    let registry =
        crate::hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = crate::db::pool::create_pool(&config_dir, &cfg)
        .context("Failed to create database pool")?;

    crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)
        .context("Failed to sync database schema")?;

    Ok((pool, registry))
}

#[cfg(test)]
mod tests {
    use super::parse_key_val;

    #[test]
    fn happy_path() {
        assert_eq!(
            parse_key_val("key=value"),
            Ok(("key".to_string(), "value".to_string()))
        );
    }

    #[test]
    fn missing_equals_returns_error() {
        let result = parse_key_val("noequals");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("no `=` found"), "unexpected error: {msg}");
    }

    #[test]
    fn multiple_equals_splits_on_first() {
        // Everything after the first `=` is the value, including additional `=` characters.
        assert_eq!(
            parse_key_val("key=val=ue"),
            Ok(("key".to_string(), "val=ue".to_string()))
        );
    }

    #[test]
    fn empty_value() {
        assert_eq!(
            parse_key_val("key="),
            Ok(("key".to_string(), String::new()))
        );
    }

    #[test]
    fn empty_key() {
        // The implementation does not reject an empty key; it returns ("", value).
        assert_eq!(
            parse_key_val("=value"),
            Ok((String::new(), "value".to_string()))
        );
    }
}
