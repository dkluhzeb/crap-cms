//! Action enums for CLI subcommands and the `parse_key_val` helper.

use clap::Subcommand;
use std::path::PathBuf;

/// Parse a key=value pair for --field arguments.
pub fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no `=` found in `{s}`"))?;

    Ok((key.to_string(), value.to_string()))
}

/// Actions for the `make` subcommand.
#[derive(Subcommand)]
pub enum MakeAction {
    /// Generate a collection Lua file
    Collection {
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

/// Actions for the `blueprint` subcommand.
#[derive(Subcommand)]
pub enum BlueprintAction {
    /// Save a config directory as a reusable blueprint
    Save {
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

/// Actions for the `user` subcommand.
#[derive(Subcommand)]
pub enum UserAction {
    /// Create a new user in an auth collection
    Create {
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
        /// Auth collection slug
        #[arg(short, long, default_value = "users")]
        collection: String,
    },

    /// Show detailed info for a single user
    Info {
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

/// Actions for the `migrate` subcommand.
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

/// Actions for the `db` subcommand.
#[derive(Subcommand)]
pub enum DbAction {
    /// Open an interactive SQLite console
    Console,
    /// Detect and optionally remove orphan columns not in Lua definitions
    Cleanup {
        /// Actually drop orphan columns (default: dry-run report only)
        #[arg(long)]
        confirm: bool,
    },
}

/// Actions for the `templates` subcommand.
///
/// Manages the user's customization layer — the files in
/// `<config_dir>/{templates,static}/` that override the compiled-in
/// defaults. `list` and `extract` are bootstrap helpers; `status` and
/// `diff` answer "what have I overridden, and is it drifting from
/// upstream?".
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
    /// Extract default files into the config directory for customization.
    /// A `crap-cms:source <version>` header is prepended (when the file
    /// type allows comments) so `templates status` can detect drift later.
    Extract {
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
    /// Report drift status for every customized file in the config dir
    Status,
    /// Show a unified diff between a customized file and the embedded default
    Diff {
        /// Path relative to the config dir
        /// (e.g. `templates/layout/base.hbs`, `static/styles.css`)
        path: String,
    },
}

/// Actions for the `jobs` subcommand.
#[derive(Subcommand)]
pub enum JobsAction {
    /// List defined jobs and recent runs
    List,
    /// Trigger a job manually
    Trigger {
        /// Job slug to trigger
        slug: String,
        /// JSON data to pass to the job (default: "{}")
        #[arg(short, long)]
        data: Option<String>,
    },
    /// Show job run history
    Status {
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
    /// Cancel pending jobs (delete from queue)
    Cancel {
        /// Only cancel jobs with this slug (default: all pending)
        #[arg(short, long)]
        slug: Option<String>,
    },
    /// Clean up old completed/failed job runs
    Purge {
        /// Delete runs older than this (e.g., "7d", "24h", "30m")
        #[arg(long, default_value = "7d")]
        older_than: String,
    },
    /// Check job system health
    Healthcheck,
}

/// Actions for the `trash` subcommand.
#[derive(Subcommand)]
pub enum TrashAction {
    /// List trashed documents
    List {
        /// Filter by collection slug
        #[arg(short, long)]
        collection: Option<String>,
    },
    /// Permanently delete trashed documents
    Purge {
        /// Filter by collection slug
        #[arg(short, long)]
        collection: Option<String>,
        /// Delete documents older than this (e.g., "30d", "24h", "30m"), or "all"
        #[arg(long, default_value = "all")]
        older_than: String,
        /// Print what would be deleted without actually deleting
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore a trashed document
    Restore {
        /// Collection slug
        collection: String,
        /// Document ID
        id: String,
    },
    /// Permanently delete all trash in a collection
    Empty {
        /// Collection slug
        collection: String,
        /// Confirm destructive operation (required)
        #[arg(short = 'y', long)]
        confirm: bool,
    },
}

/// Actions for the `images` subcommand.
#[derive(Subcommand)]
pub enum ImagesAction {
    /// List image processing queue entries
    List {
        /// Filter by status: pending, processing, completed, failed
        #[arg(short, long)]
        status: Option<String>,

        /// Max entries to show
        #[arg(short, long, default_value = "20")]
        limit: i64,
    },
    /// Show queue statistics by status
    Stats,
    /// Retry failed queue entries
    Retry {
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
        /// Delete entries older than this (e.g., "7d", "24h", "30m")
        #[arg(long, default_value = "7d")]
        older_than: String,
    },
}

/// Actions for the `bench` subcommand.
#[derive(Subcommand)]
pub enum BenchAction {
    /// Time individual Lua hooks (interactive selection by default)
    Hooks {
        /// Filter to a specific collection
        #[arg(short, long)]
        collection: Option<String>,

        /// Number of iterations per hook
        #[arg(short = 'n', long, default_value = "10")]
        iterations: usize,

        /// Run only these hooks (comma-separated function refs)
        #[arg(long)]
        hooks: Option<String>,

        /// Run all hooks except these (comma-separated function refs)
        #[arg(long)]
        exclude: Option<String>,

        /// Run all hooks (skip interactive selection). WARNING: hooks may have side effects.
        #[arg(long)]
        all: bool,

        /// Input data as JSON object (overrides automatic data resolution)
        #[arg(short, long)]
        data: Option<String>,
    },

    /// Time find queries on each collection
    Queries {
        /// Filter to a specific collection
        #[arg(short, long)]
        collection: Option<String>,

        /// Show EXPLAIN QUERY PLAN output (SQLite only)
        #[arg(long)]
        explain: bool,

        /// JSON filter clause (same format as gRPC `where` parameter)
        #[arg(short, long)]
        r#where: Option<String>,
    },

    /// Time a full document create cycle (transaction is rolled back)
    Create {
        /// Collection slug to benchmark
        collection: String,

        /// Number of iterations
        #[arg(short = 'n', long, default_value = "5")]
        iterations: usize,

        /// Input data as JSON object
        #[arg(short, long)]
        data: Option<String>,

        /// Skip hooks (measure pure validation + persist)
        #[arg(long)]
        no_hooks: bool,

        /// Skip confirmation prompt for hook side effects
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

/// Actions for the `logs` subcommand.
#[derive(Subcommand)]
pub enum LogsAction {
    /// Remove old rotated log files (keeps the current log file)
    Clear,
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

    /// Regression: multi-byte UTF-8 in key or value must not panic from string slicing.
    #[test]
    fn multibyte_utf8_does_not_panic() {
        assert_eq!(
            parse_key_val("clé=valeur"),
            Ok(("clé".to_string(), "valeur".to_string()))
        );
        assert_eq!(
            parse_key_val("key=日本語"),
            Ok(("key".to_string(), "日本語".to_string()))
        );
        assert_eq!(
            parse_key_val("キー=値"),
            Ok(("キー".to_string(), "値".to_string()))
        );
    }
}
