//! `typegen` and `proto` commands.

use anyhow::{Context as _, Result, anyhow};
use std::path::Path;

use crate::{cli, config, hooks, typegen};

/// Handle the `typegen` subcommand — loads the Lua registry and generates types.
pub fn run(
    config_dir: &Path,
    lang_str: &str,
    output_dir: Option<&Path>,
    proto_mod: Option<&str>,
) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    // Load config + Lua VM to get registry
    let cfg = config::CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    if lang_str == "all" {
        for lang in typegen::Language::all() {
            let path = typegen::generate_lang(&config_dir, &reg, *lang, output_dir)
                .with_context(|| format!("Failed to generate {} types", lang.label()))?;

            cli::success(&format!("Generated {}", path.display()));
        }
    } else {
        let lang = typegen::Language::from_name(lang_str).ok_or_else(|| {
            anyhow!(
                "Unknown language '{}'. Valid: lua, ts, go, py, rs, all",
                lang_str
            )
        })?;

        let path = typegen::generate_lang(&config_dir, &reg, lang, output_dir)
            .context("Failed to generate type definitions")?;

        cli::success(&format!("Generated {}", path.display()));
    }

    // Generate proto conversion code if requested (Rust only)
    if let Some(proto_path) = proto_mod {
        let path = typegen::generate_proto_conversion(&config_dir, &reg, proto_path, output_dir)
            .context("Failed to generate proto conversion code")?;

        cli::success(&format!("Generated {}", path.display()));
    }

    Ok(())
}
