//! `init` command — scaffold a new config directory.

use std::{fs, path::PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use crate::scaffold::render::render;

// ── Static files (compiled in, no templating needed) ─────────────────────

const STATIC_INIT_LUA: &str = include_str!("templates/init.lua.tpl");
const STATIC_LUARC: &str = include_str!("templates/luarc.json.tpl");
const STATIC_GITIGNORE: &str = include_str!("templates/gitignore.tpl");
const STATIC_STYLUA: &str = include_str!("templates/stylua.toml.tpl");

/// Embedded Lua API type definitions — compiled into the binary.
pub(crate) const LUA_API_TYPES: &str = include_str!("../../../types/crap.lua");

// ── Types ────────────────────────────────────────────────────────────────

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

/// Directories scaffolded inside the config root.
const SUBDIRS: &[&str] = &[
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
];

// ── Public entry point ───────────────────────────────────────────────────

/// Scaffold a new config directory with minimum viable structure.
///
/// Creates: crap.toml, init.lua, .luarc.json, .gitignore, stylua.toml,
/// and empty directories for collections, globals, hooks, etc.
pub fn init(dir: Option<PathBuf>, opts: &InitOptions) -> Result<()> {
    let target = dir.unwrap_or_else(|| PathBuf::from("./crap-cms"));

    if target.join("crap.toml").exists() {
        bail!(
            "Directory '{}' already contains a crap.toml — refusing to overwrite",
            target.display()
        );
    }

    create_directories(&target)?;
    write_scaffolded_files(&target, opts)?;

    let abs = target.canonicalize().unwrap_or_else(|_| target.clone());
    println!("Scaffolded config directory: {}", abs.display());

    Ok(())
}

/// Create the target directory and all subdirectories.
fn create_directories(target: &std::path::Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("Failed to create directory '{}'", target.display()))?;

    for subdir in SUBDIRS {
        fs::create_dir_all(target.join(subdir))
            .with_context(|| format!("Failed to create {}/", subdir))?;
    }

    Ok(())
}

/// Write all scaffolded files into the target directory.
fn write_scaffolded_files(target: &std::path::Path, opts: &InitOptions) -> Result<()> {
    let toml = render_crap_toml(opts)?;

    fs::write(target.join("crap.toml"), &toml).context("Failed to write crap.toml")?;
    fs::write(target.join("types/crap.lua"), LUA_API_TYPES)
        .context("Failed to write types/crap.lua")?;
    fs::write(target.join("init.lua"), STATIC_INIT_LUA).context("Failed to write init.lua")?;
    fs::write(target.join(".luarc.json"), STATIC_LUARC).context("Failed to write .luarc.json")?;
    fs::write(target.join(".gitignore"), STATIC_GITIGNORE).context("Failed to write .gitignore")?;
    fs::write(target.join("stylua.toml"), STATIC_STYLUA).context("Failed to write stylua.toml")?;

    Ok(())
}

/// Render the `crap.toml` config via Handlebars.
fn render_crap_toml(opts: &InitOptions) -> Result<String> {
    let locales_str = opts
        .locales
        .iter()
        .map(|l| format!("\"{}\"", l))
        .collect::<Vec<_>>()
        .join(", ");

    render(
        "crap_toml",
        &json!({
            "version": env!("CARGO_PKG_VERSION"),
            "admin_port": opts.admin_port,
            "grpc_port": opts.grpc_port,
            "auth_secret": opts.auth_secret,
            "has_locales": !opts.locales.is_empty(),
            "default_locale": opts.default_locale,
            "locales_str": locales_str,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_structure() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("my-project");
        init(Some(target.clone()), &InitOptions::default()).unwrap();

        assert!(target.join("crap.toml").exists());
        assert!(target.join("init.lua").exists());
        assert!(target.join(".luarc.json").exists());
        assert!(target.join(".gitignore").exists());
        assert!(target.join("stylua.toml").exists());
        assert!(target.join("types/crap.lua").exists());

        for subdir in SUBDIRS {
            assert!(target.join(subdir).is_dir(), "{subdir}/ should exist");
        }
    }

    #[test]
    fn generates_dynamic_toml() {
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
        assert!(content.contains("# [locale]"));

        for section in &[
            "# [depth]",
            "# [pagination]",
            "# [upload]",
            "# [email]",
            "# [hooks]",
            "# [jobs]",
            "# [cors]",
            "# [access]",
            "# [logging]",
            "# [mcp]",
        ] {
            assert!(content.contains(section), "missing section: {section}");
        }
    }

    #[test]
    fn with_locales() {
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
    fn with_single_locale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("single_locale");
        let opts = InitOptions {
            locales: vec!["en".to_string()],
            default_locale: "en".to_string(),
            ..InitOptions::default()
        };
        init(Some(target.clone()), &opts).unwrap();

        let content = fs::read_to_string(target.join("crap.toml")).unwrap();
        assert!(content.contains("[locale]"));
        assert!(content.contains("locales = [\"en\"]"));
    }

    #[test]
    fn types_lua_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("types_check");
        init(Some(target.clone()), &InitOptions::default()).unwrap();

        let content = fs::read_to_string(target.join("types/crap.lua")).unwrap();
        assert!(!content.is_empty());
        assert!(content.contains("crap"));
        assert!(content.contains("collections"));
    }

    #[test]
    fn refuses_overwrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("existing");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("crap.toml"), "# existing").unwrap();

        let result = init(Some(target), &InitOptions::default());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("refusing to overwrite")
        );
    }

    #[test]
    fn stylua_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("stylua_check");
        init(Some(target.clone()), &InitOptions::default()).unwrap();

        let content = fs::read_to_string(target.join("stylua.toml")).unwrap();
        assert!(content.contains("indent_type"));
        assert!(content.contains("indent_width"));
    }
}
