//! `make theme` — generate a theme CSS file at
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

use anyhow::{Context as _, Result, bail};

use crate::{cli, scaffold::validate_template_slug};

/// Options for `make_theme`.
pub struct MakeThemeOptions<'a> {
    pub config_dir: &'a Path,
    pub name: &'a str,
    pub force: bool,
}

/// Scaffold a theme CSS file in `static/styles/themes/themes-<name>.css`.
pub fn make_theme(opts: &MakeThemeOptions) -> Result<()> {
    validate_template_slug(opts.name)?;

    let dir = opts.config_dir.join("static").join("styles").join("themes");
    fs::create_dir_all(&dir).context("Failed to create static/styles/themes/ directory")?;

    let file_path = dir.join(format!("themes-{}.css", opts.name));

    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let css = render_theme_css(opts.name);

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

/// Render the CSS file body. The output is intentionally a fully-commented
/// token catalogue — uncomment lines to override individual tokens.
/// Matches the structure of `static/styles/tokens.css` so a side-by-side
/// view is straightforward.
fn render_theme_css(name: &str) -> String {
    format!(
        r#"/**
 * Theme: {name}
 *
 * Activates on `<html data-theme="{name}">`. Override tokens below by
 * uncommenting and tweaking values. Anything you leave commented falls
 * back to the default theme (see `static/styles/themes/default.css`).
 *
 * Token catalogue mirrors `static/styles/tokens.css`. See
 * `docs/src/admin-ui/reference/css-variables.md` for the contract per
 * token.
 */

html[data-theme="{name}"] {{
  /* color-scheme: light;  /* or "dark" */

  /* ── Brand ──────────────────────────────────────────────────── */
  /* --color-primary:        #1677ff; */
  /* --color-primary-hover:  #4096ff; */
  /* --color-primary-active: #0958d9; */
  /* --color-primary-bg:     rgba(22, 119, 255, 0.06); */

  /* ── Status colours ─────────────────────────────────────────── */
  /* --color-danger:    #ff4d4f; */
  /* --color-success:   #52c41a; */
  /* --color-warning:   #faad14; */

  /* ── Text ───────────────────────────────────────────────────── */
  /* --text-primary:    rgba(0, 0, 0, 0.88); */
  /* --text-secondary:  rgba(0, 0, 0, 0.65); */
  /* --text-tertiary:   rgba(0, 0, 0, 0.45); */

  /* ── Surfaces ───────────────────────────────────────────────── */
  /* --bg-body:      #f4f7fc; */
  /* --bg-surface:   #f8f9fb; */
  /* --bg-elevated:  #fff; */

  /* ── Radii (uncomment to flatten/round the entire admin) ────── */
  /* --radius-sm: 4px; */
  /* --radius-md: 6px; */
  /* --radius-lg: 8px; */

  /* ── Fonts ──────────────────────────────────────────────────── */
  /* --font-family: "Inter", system-ui, -apple-system, sans-serif; */
}}
"#,
        name = name,
    )
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
