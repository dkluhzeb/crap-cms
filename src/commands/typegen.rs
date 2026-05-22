//! `typegen` subcommand handlers. Three artifacts, three subcommands:
//! `lua` for server-side hook authors, `client` for external API
//! consumers, `proto` for Rust gRPC implementations.

use std::path::Path;

use anyhow::{Context as _, Result, anyhow};

use crate::{
    cli, config, hooks,
    typegen::{self, Language},
};

use super::TypegenAction;

/// Dispatch a `typegen` invocation to the matching artifact handler.
/// All three subcommands load the same `(CrapConfig, Registry)` pair
/// up-front so the typegen entry points downstream don't have to.
///
/// # Errors
///
/// Returns an error if config loading or Lua VM initialization fails,
/// if a `client` language string is unknown, or if any output file can't
/// be written.
pub fn run(config_dir: &Path, action: TypegenAction) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = config::CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;

    match action {
        TypegenAction::Lua { output } => {
            let paths = typegen::generate_lua(&config_dir, &registry, output.as_deref())
                .context("Failed to generate Lua types")?;
            for path in paths {
                cli::success(&format!("Generated {}", path.display()));
            }
            Ok(())
        }
        TypegenAction::Client { lang, output } => {
            if lang.is_empty() {
                return Err(anyhow!(
                    "no languages specified — pass `--lang ts[,go,py,rs]`"
                ));
            }
            for lang_str in &lang {
                let parsed = Language::from_name(lang_str).ok_or_else(|| {
                    anyhow!("unknown client language '{lang_str}' — supported: ts, go, py, rs")
                })?;
                let path =
                    typegen::generate_client(&config_dir, &registry, parsed, output.as_deref())
                        .with_context(|| {
                            format!("Failed to generate {} client types", parsed.label())
                        })?;
                cli::success(&format!("Generated {}", path.display()));
            }
            Ok(())
        }
        TypegenAction::Proto { module, output } => {
            let path = typegen::generate_proto(&config_dir, &registry, &module, output.as_deref())
                .context("Failed to generate proto conversion code")?;
            cli::success(&format!("Generated {}", path.display()));
            Ok(())
        }
    }
}
