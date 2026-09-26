//! `crap-cms check` — validate a project's `crap.toml` and definitions
//! without opening its database.
//!
//! The same checks a start runs before it touches the database: `crap.toml`
//! decoded and validated, then every definition file loaded and every boot
//! gate run over them, then the generated identifiers checked — each stage
//! reporting every problem it finds at once.
//! Nothing is written into the project (no generated auth secret, no
//! database), so it is safe to run against a project before swapping in a
//! new binary, and repeatedly while fixing what it reports.

use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{cli, config::CrapConfig, db::migrate, hooks};

/// Check the project at `config_dir`: its configuration first (the
/// definitions are loaded under it), then its definitions.
///
/// # Errors
///
/// Returns an error listing every configuration problem, or — with a valid
/// configuration — every definition problem.
pub fn run(config_dir: &Path) -> Result<()> {
    let config = CrapConfig::check(config_dir).context("crap.toml has problems")?;
    config.apply().context("crap.toml has problems")?;

    let registry = hooks::init_lua(config_dir, &config).context("The definitions have problems")?;

    // The schema sync's own first check, which needs no database: every
    // generated table, column and index name fits the backends' limits.
    migrate::check_all_identifiers(&registry, &config.locale)
        .context("The definitions have problems")?;

    cli::success(&format!(
        "crap.toml and the definitions are valid ({} collection(s), {} global(s))",
        registry.collections.len(),
        registry.globals.len()
    ));

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A valid project passes, and the check writes nothing into it.
    #[test]
    fn a_valid_project_passes_and_is_left_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();

        run(tmp.path()).expect("an empty project is valid");

        assert!(!tmp.path().join("data").exists(), "nothing is written");
    }

    /// Every broken definition file is reported, not only the first.
    #[test]
    fn every_broken_definition_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        fs::create_dir_all(tmp.path().join("globals")).unwrap();
        fs::write(tmp.path().join("globals/one.lua"), "error('one')").unwrap();
        fs::write(tmp.path().join("globals/two.lua"), "error('two')").unwrap();

        let err = format!("{:#}", run(tmp.path()).unwrap_err());

        assert!(err.contains("one.lua") && err.contains("two.lua"), "{err}");
    }

    /// A generated identifier too long for Postgres is reported without a
    /// database — the check a start would otherwise only run when it syncs.
    #[test]
    fn an_overlong_identifier_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        fs::create_dir_all(tmp.path().join("collections")).unwrap();
        let long = "a".repeat(70);
        fs::write(
            tmp.path().join("collections/posts.lua"),
            format!(
                "crap.collections.define('posts', {{ fields = {{ {{ name = '{long}', type = 'text' }} }} }})"
            ),
        )
        .unwrap();

        let err = format!("{:#}", run(tmp.path()).unwrap_err());

        assert!(err.contains(&long), "{err}");
    }
}
