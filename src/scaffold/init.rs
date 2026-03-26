//! `init` command — scaffold a new config directory.

use anyhow::{Context as _, Result, bail};
use std::{fs, path::PathBuf};

/// Embedded Lua API type definitions — compiled into the binary.
pub(crate) const LUA_API_TYPES: &str = include_str!("../../types/crap.lua");

/// Options for `init()`. Controls what gets written to `crap.toml`.
pub struct InitOptions {
    pub admin_port: u16,
    pub grpc_port: u16,
    pub locales: Vec<String>,
    pub default_locale: String,
    pub auth_secret: String,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            admin_port: 3000,
            grpc_port: 50051,
            locales: vec![],
            default_locale: "en".to_string(),
            auth_secret: nanoid::nanoid!(64),
        }
    }
}

/// Scaffold a new config directory with minimum viable structure.
///
/// Creates: crap.toml, init.lua, .luarc.json, .gitignore, and empty directories
/// for collections, globals, hooks, templates, and static.
///
/// Refuses to overwrite if the directory already contains a crap.toml.
pub fn init(dir: Option<PathBuf>, opts: &InitOptions) -> Result<()> {
    let target = dir.unwrap_or_else(|| PathBuf::from("./crap-cms"));

    // Refuse to overwrite existing config
    let toml_path = target.join("crap.toml");

    if toml_path.exists() {
        bail!(
            "Directory '{}' already contains a crap.toml — refusing to overwrite",
            target.display()
        );
    }

    // Create the directory structure
    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create directory '{}'", target.display()))?;

    for subdir in &[
        "collections",
        "globals",
        "hooks",
        "access",
        "jobs",
        "plugins",
        "templates",
        "static",
        "migrations",
        "types",
    ] {
        fs::create_dir_all(target.join(subdir))
            .with_context(|| format!("Failed to create {}/", subdir))?;
    }

    // Write embedded Lua API type definitions
    fs::write(target.join("types/crap.lua"), LUA_API_TYPES)
        .context("Failed to write types/crap.lua")?;

    // Build crap.toml dynamically from InitOptions
    let mut toml = String::new();
    toml.push_str(&format!(
        "crap_version = \"{}\"\n\n[server]\nadmin_port = {}\ngrpc_port = {}\nhost = \"0.0.0.0\"\n# compression = \"off\"             # \"off\" (default), \"gzip\", \"br\", \"all\"\n# grpc_reflection = false          # enable gRPC server reflection (default: false)\n# grpc_rate_limit_requests = 0    # per-IP request limit (0 = disabled)\n# grpc_rate_limit_window = 60     # sliding window in seconds (or \"1m\")\n# grpc_max_message_size = \"16MB\"  # max gRPC message size (default 16MB)\n",
        env!("CARGO_PKG_VERSION"), opts.admin_port, opts.grpc_port
    ));
    toml.push_str("\n[database]\npath = \"data/crap.db\"\n# pool_max_size = 32             # max connections in pool\n# busy_timeout = \"30s\"          # SQLite busy timeout (ms or \"30s\", \"1m\")\n# connection_timeout = 5          # pool checkout timeout (seconds or \"5s\")\n");
    toml.push_str("\n[admin]\ndev_mode = false\n# require_auth = true               # block admin when no auth collection exists (default: true)\n# access = \"access.admin_panel\"     # Lua function: which users can access the admin UI\n\n# [admin.csp]                       # Content-Security-Policy header (enabled by default)\n# enabled = true\n# script_src = [\"'self'\", \"'unsafe-inline'\", \"https://unpkg.com\"]\n# style_src = [\"'self'\", \"'unsafe-inline'\", \"https://fonts.googleapis.com\"]\n# font_src = [\"'self'\", \"https://fonts.gstatic.com\"]\n# img_src = [\"'self'\", \"data:\"]\n# connect_src = [\"'self'\"]\n# frame_ancestors = [\"'none'\"]\n# form_action = [\"'self'\"]\n# base_uri = [\"'self'\"]\n");
    toml.push_str(&format!(
        "\n[auth]\nsecret = \"{}\"\n# token_expiry = 7200              # seconds, default 2 hours\n# max_login_attempts = 5           # failed logins before lockout\n# login_lockout_seconds = 300      # lockout duration (5 minutes)\n# reset_token_expiry = 3600        # password reset token lifetime (1 hour)\n# max_forgot_password_attempts = 3 # rate limit forgot-password per email\n# forgot_password_window_seconds = 900  # rate limit window (15 minutes)\n",
        opts.auth_secret
    ));
    toml.push_str("\n# [auth.password_policy]\n# min_length = 12               # minimum password length\n# max_length = 128              # maximum password length\n# require_uppercase = false     # require uppercase letter\n# require_lowercase = false     # require lowercase letter\n# require_digit = false         # require digit\n# require_special = false       # require special character\n");
    toml.push_str("\n[live]\n# enabled = true                # enable SSE + gRPC Subscribe for live mutation events\n# channel_capacity = 1024       # broadcast channel buffer size\n# max_sse_connections = 1000    # max concurrent SSE connections (0 = unlimited)\n# max_subscribe_connections = 1000  # max concurrent gRPC Subscribe streams (0 = unlimited)\n");

    if opts.locales.is_empty() {
        toml.push_str("\n# [locale]\n# default_locale = \"en\"         # default locale for content\n# locales = [\"en\", \"de\"]        # supported locales (empty = disabled)\n# fallback = true               # fall back to default locale if field is empty\n");
    } else {
        let locales_str = opts
            .locales
            .iter()
            .map(|l| format!("\"{}\"", l))
            .collect::<Vec<_>>()
            .join(", ");
        toml.push_str(&format!(
            "\n[locale]\ndefault_locale = \"{}\"\nlocales = [{}]\nfallback = true\n",
            opts.default_locale, locales_str
        ));
    }

    toml.push_str("\n# [depth]\n# default_depth = 1              # default relationship population depth for FindByID\n# max_depth = 10                 # hard cap on population depth\n# populate_cache = false          # enable cross-request populate cache\n# populate_cache_max_age_secs = 0 # max age in seconds for populate cache (0 = indefinite)\n");
    toml.push_str("\n# [pagination]\n# default_limit = 20            # default page size when no limit specified\n# max_limit = 1000               # maximum allowed limit (requests above this are clamped)\n# mode = \"page\"                  # pagination mode: \"page\" (offset-based) or \"cursor\" (keyset-based)\n");
    toml.push_str("\n# [upload]\n# max_file_size = \"50MB\"         # global max upload size (integer bytes or \"50MB\", \"1GB\")\n");
    toml.push_str("\n# [email]\n# smtp_host = \"\"                 # SMTP server (empty = email disabled)\n# smtp_port = 587\n# smtp_user = \"\"\n# smtp_pass = \"\"\n# smtp_tls = \"starttls\"           # \"starttls\" (default), \"tls\" (implicit), \"none\" (plain)\n# from_address = \"noreply@example.com\"\n# from_name = \"Crap CMS\"\n# smtp_timeout = 30               # SMTP connection/send timeout (seconds or \"30s\")\n");
    toml.push_str("\n# [hooks]\n# on_init = []                   # hook functions to run at startup\n# max_depth = 3                  # max hook recursion depth (hook > CRUD > hook)\n# vm_pool_size = 8               # number of Lua VMs for concurrent hook execution (default: max(cpus, 4), cap 32)\n# max_instructions = 10000000    # max Lua instructions per hook invocation\n# max_memory = \"50MB\"            # max Lua memory per VM\n# allow_private_networks = false # allow Lua HTTP requests to private/internal networks\n# http_max_response_bytes = \"10MB\" # max HTTP response body size for crap.http.request\n");
    toml.push_str("\n# [jobs]\n# max_concurrent = 10             # max concurrent job executions\n# poll_interval = 1              # seconds between job queue polls\n# cron_interval = 60             # seconds between cron schedule checks\n# heartbeat_interval = 10        # seconds between running job heartbeats\n# auto_purge = 604800            # auto-purge completed/failed jobs older than N seconds (7 days)\n# image_queue_batch_size = 10    # image conversions per scheduler poll\n");
    toml.push_str("\n# [cors]\n# allowed_origins = []           # empty = CORS disabled; [\"*\"] = allow any origin\n# allowed_methods = [\"GET\", \"POST\", \"PUT\", \"DELETE\", \"PATCH\", \"OPTIONS\"]\n# allowed_headers = [\"Content-Type\", \"Authorization\"]\n# exposed_headers = []           # response headers exposed to the browser\n# max_age = 3600                   # seconds or human-readable (\"1h\", \"30m\")\n# allow_credentials = false      # cannot be used with wildcard origin\n");
    toml.push_str("\n# [access]\n# default_deny = false           # when true, collections without access functions deny all\n");
    toml.push_str("\n# [mcp]\n# enabled = false                # enable MCP (Model Context Protocol) server\n# http = false                   # enable HTTP transport on /mcp (POST)\n# config_tools = false           # enable config generation tools (write access to config dir)\n# api_key = \"\"                   # API key for HTTP transport auth (empty = no auth)\n# include_collections = []       # whitelist (empty = all)\n# exclude_collections = []       # blacklist (takes precedence over include)\n");

    fs::write(&toml_path, &toml).context("Failed to write crap.toml")?;

    // init.lua — entry point with commented examples
    fs::write(
        target.join("init.lua"),
        r#"-- init.lua -- runs once at startup.
-- Register global hooks, load plugins, or set up shared state.

-- Example: register a global hook that runs for ALL collections
-- crap.hooks.register("after_change", function(context)
--     crap.log.info("Document changed: " .. context.collection .. "/" .. (context.data.id or ""))
-- end)

-- Example: load a plugin module
-- require("plugins.seo")

crap.log.info("init.lua loaded")
"#,
    )
    .context("Failed to write init.lua")?;

    // .luarc.json — LuaLS config pointing to types/
    fs::write(
        target.join(".luarc.json"),
        r#"{
  "workspace.library": ["./types"],
  "runtime.version": "Lua 5.4",
  "diagnostics.globals": ["crap"]
}
"#,
    )
    .context("Failed to write .luarc.json")?;

    // .gitignore — data and uploads (runtime artifacts)
    fs::write(
        target.join(".gitignore"),
        "data/\nuploads/\ntypes/\ndata/.jwt_secret\n",
    )
    .context("Failed to write .gitignore")?;

    // stylua.toml — Lua formatter config
    fs::write(
        target.join("stylua.toml"),
        "indent_type = \"Spaces\"\nindent_width = 2\n",
    )
    .context("Failed to write stylua.toml")?;

    let abs = target.canonicalize().unwrap_or_else(|_| target.clone());
    println!("Scaffolded config directory: {}", abs.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_creates_structure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("my-project");
        init(Some(target.clone()), &InitOptions::default()).unwrap();

        assert!(target.join("crap.toml").exists());
        assert!(target.join("init.lua").exists());
        assert!(target.join(".luarc.json").exists());
        assert!(target.join(".gitignore").exists());
        assert!(target.join("collections").is_dir());
        assert!(target.join("globals").is_dir());
        assert!(target.join("hooks").is_dir());
        assert!(target.join("access").is_dir());
        assert!(target.join("jobs").is_dir());
        assert!(target.join("plugins").is_dir());
        assert!(target.join("templates").is_dir());
        assert!(target.join("static").is_dir());
        assert!(target.join("migrations").is_dir());
        assert!(target.join("types").is_dir());
        assert!(target.join("types/crap.lua").exists());
        assert!(target.join("stylua.toml").exists());
        let stylua = fs::read_to_string(target.join("stylua.toml")).unwrap();
        assert!(stylua.contains("indent_type"));
        assert!(stylua.contains("indent_width"));
    }

    #[test]
    fn test_init_generates_dynamic_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("dynamic");
        let opts = InitOptions {
            admin_port: 4000,
            grpc_port: 50052,
            locales: vec![],
            default_locale: "en".to_string(),
            auth_secret: "test-secret-123".to_string(),
        };
        init(Some(target.clone()), &opts).unwrap();

        let content = fs::read_to_string(target.join("crap.toml")).unwrap();
        assert!(content.contains(&format!("crap_version = \"{}\"", env!("CARGO_PKG_VERSION"))));
        assert!(content.contains("admin_port = 4000"));
        assert!(content.contains("grpc_port = 50052"));
        assert!(content.contains("secret = \"test-secret-123\""));
        // No active [locale] section when locales is empty
        assert!(content.contains("# [locale]"));
        // Commented config sections for discoverability
        assert!(content.contains("# [depth]"));
        assert!(content.contains("# [pagination]"));
        assert!(content.contains("# [upload]"));
        assert!(content.contains("# [email]"));
        assert!(content.contains("# [hooks]"));
        assert!(content.contains("# [jobs]"));
        assert!(content.contains("# [cors]"));
        assert!(content.contains("# [access]"));
        assert!(content.contains("# [mcp]"));
    }

    #[test]
    fn test_init_with_locales() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("localized");
        let opts = InitOptions {
            admin_port: 3000,
            grpc_port: 50051,
            locales: vec!["en".to_string(), "de".to_string(), "fr".to_string()],
            default_locale: "en".to_string(),
            auth_secret: "secret".to_string(),
        };
        init(Some(target.clone()), &opts).unwrap();

        let content = fs::read_to_string(target.join("crap.toml")).unwrap();
        assert!(content.contains("[locale]"));
        assert!(content.contains("default_locale = \"en\""));
        assert!(content.contains("locales = [\"en\", \"de\", \"fr\"]"));
        assert!(content.contains("fallback = true"));
    }

    #[test]
    fn test_init_with_single_locale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("single_locale");
        let opts = InitOptions {
            locales: vec!["en".to_string()],
            default_locale: "en".to_string(),
            ..InitOptions::default()
        };
        init(Some(target.clone()), &opts).unwrap();

        let content = fs::read_to_string(target.join("crap.toml")).unwrap();
        // Single locale still gets active [locale] section
        assert!(content.contains("[locale]"));
        assert!(content.contains("default_locale = \"en\""));
        assert!(content.contains("locales = [\"en\"]"));
        assert!(content.contains("fallback = true"));
    }

    #[test]
    fn test_init_types_lua_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("types_check");
        init(Some(target.clone()), &InitOptions::default()).unwrap();

        let content = fs::read_to_string(target.join("types/crap.lua")).unwrap();
        assert!(!content.is_empty(), "types/crap.lua should not be empty");
        // Should contain key Lua API markers
        assert!(
            content.contains("crap"),
            "types/crap.lua should reference 'crap' global"
        );
        assert!(
            content.contains("collections"),
            "types/crap.lua should reference collections API"
        );
        assert!(
            content.contains("fields"),
            "types/crap.lua should reference fields API"
        );
    }

    #[test]
    fn test_init_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("existing");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("crap.toml"), "# existing").unwrap();

        let result = init(Some(target), &InitOptions::default());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("refusing to overwrite")
        );
    }
}
