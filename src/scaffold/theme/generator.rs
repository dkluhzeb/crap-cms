//! `make theme` -- generate a theme CSS file at
//! `static/styles/themes/themes-<name>.css` with the documented token
//! catalogue commented out, ready for the user to uncomment + tweak.
//!
//! Activation flow once the file exists:
//!   1. The user adds `@import url("/static/styles/themes/themes-<name>.css");`
//!      to their `<config_dir>/static/styles/main.css` overlay (or the
//!      built-in main.css if shadowing it).
//!   2. The theme switches in via `localStorage.setItem('crap-theme', '<name>')`
//!      or `window.crap.theme.set('<name>')`.

use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    cli,
    scaffold::{guards::refuse_file_overwrite, paths, render, validate_template_slug},
};

#[derive(Serialize)]
struct ThemeCtx<'a> {
    name: &'a str,
}

/// Options for `make_theme`.
pub struct MakeThemeOptions<'a> {
    pub config_dir: &'a Path,
    pub name: &'a str,
    pub force: bool,
}

/// Scaffold a theme CSS file in `static/styles/themes/themes-<name>.css`.
///
/// # Errors
///
/// Returns an error if the name is invalid, the file already exists without
/// `--force`, or writing fails.
pub fn make_theme(opts: &MakeThemeOptions) -> Result<()> {
    validate_template_slug(opts.name)?;

    let dir = paths::static_themes_dir(opts.config_dir);
    fs::create_dir_all(&dir).context("Failed to create static/styles/themes/ directory")?;

    let file_path = dir.join(format!("themes-{}.css", opts.name));

    refuse_file_overwrite(&file_path, opts.force)?;

    let css = render_theme_css(opts.name)?;

    fs::write(&file_path, &css)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));
    cli::info(&format!(
        "Activate via `localStorage.setItem('crap-theme', '{}')` or `window.crap.theme.set('{}')`.",
        opts.name, opts.name,
    ));
    cli::info("Add an @import to your main.css overlay so the file is loaded.");

    Ok(())
}

fn render_theme_css(name: &str) -> Result<String> {
    render::render("theme", &ThemeCtx { name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_themes_prefixed_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_theme(&MakeThemeOptions {
            config_dir: tmp.path(),
            name: "acme",
            force: false,
        })
        .expect("make_theme");

        let file = tmp.path().join("static/styles/themes/themes-acme.css");
        assert!(file.exists(), "themes-acme.css must be created");
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains(r#"html[data-theme="acme"]"#));
        assert!(body.contains("--color-primary"));
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = MakeThemeOptions {
            config_dir: tmp.path(),
            name: "acme",
            force: false,
        };
        make_theme(&opts).unwrap();
        let err = make_theme(&opts).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn force_overwrites_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("static/styles/themes/themes-acme.css");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "OLD").unwrap();
        make_theme(&MakeThemeOptions {
            config_dir: tmp.path(),
            name: "acme",
            force: true,
        })
        .unwrap();
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains(r#"html[data-theme="acme"]"#));
        assert!(!body.contains("OLD"));
    }

    #[test]
    fn rejects_invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = make_theme(&MakeThemeOptions {
            config_dir: tmp.path(),
            name: "../etc",
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("invalid"));
    }

    #[test]
    fn accepts_hyphenated_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_theme(&MakeThemeOptions {
            config_dir: tmp.path(),
            name: "acme-dark",
            force: false,
        })
        .unwrap();
        assert!(
            tmp.path()
                .join("static/styles/themes/themes-acme-dark.css")
                .exists()
        );
    }
}
