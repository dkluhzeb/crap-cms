//! `init` command — scaffold a new config directory.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

/// Embedded Lua API type definitions — compiled into the binary.
const LUA_API_TYPES: &str = include_str!("../../types/crap.lua");

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
        anyhow::bail!(
            "Directory '{}' already contains a crap.toml — refusing to overwrite",
            target.display()
        );
    }

    // Create the directory structure
    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create directory '{}'", target.display()))?;

    for subdir in &["collections", "globals", "hooks", "templates", "static", "migrations", "types"] {
        fs::create_dir_all(target.join(subdir))
            .with_context(|| format!("Failed to create {}/", subdir))?;
    }

    // Write embedded Lua API type definitions
    fs::write(target.join("types/crap.lua"), LUA_API_TYPES)
        .context("Failed to write types/crap.lua")?;

    // Build crap.toml dynamically from InitOptions
    let mut toml = String::new();
    toml.push_str(&format!(
        "[server]\nadmin_port = {}\ngrpc_port = {}\nhost = \"0.0.0.0\"\n",
        opts.admin_port, opts.grpc_port
    ));
    toml.push_str("\n[database]\npath = \"data/crap.db\"\n");
    toml.push_str("\n[admin]\ndev_mode = true\n");
    toml.push_str(&format!(
        "\n[auth]\nsecret = \"{}\"\n# token_expiry = 7200           # seconds, default 2 hours\n",
        opts.auth_secret
    ));
    toml.push_str("\n[live]\n# enabled = true                # enable SSE + gRPC Subscribe for live mutation events\n# channel_capacity = 1024       # broadcast channel buffer size\n");

    if opts.locales.is_empty() {
        toml.push_str("\n# [locale]\n# default_locale = \"en\"         # default locale for content\n# locales = [\"en\", \"de\"]        # supported locales (empty = disabled)\n# fallback = true               # fall back to default locale if field is empty\n");
    } else {
        let locales_str = opts.locales.iter()
            .map(|l| format!("\"{}\"", l))
            .collect::<Vec<_>>()
            .join(", ");
        toml.push_str(&format!(
            "\n[locale]\ndefault_locale = \"{}\"\nlocales = [{}]\nfallback = true\n",
            opts.default_locale, locales_str
        ));
    }

    fs::write(&toml_path, &toml)
        .context("Failed to write crap.toml")?;

    // init.lua — empty entry point with comments
    fs::write(
        target.join("init.lua"),
        r#"-- init.lua — runs once at startup.
-- Register global hooks, set up shared state, or log startup info.

crap.log.info("Crap CMS initializing...")

crap.log.info("init.lua loaded successfully")
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
        assert!(target.join("templates").is_dir());
        assert!(target.join("static").is_dir());
        assert!(target.join("migrations").is_dir());
        assert!(target.join("types").is_dir());
        assert!(target.join("types/crap.lua").exists());
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
        assert!(content.contains("admin_port = 4000"));
        assert!(content.contains("grpc_port = 50052"));
        assert!(content.contains("secret = \"test-secret-123\""));
        // No active [locale] section when locales is empty
        assert!(content.contains("# [locale]"));
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
    fn test_init_refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("existing");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("crap.toml"), "# existing").unwrap();

        let result = init(Some(target), &InitOptions::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("refusing to overwrite"));
    }
}
