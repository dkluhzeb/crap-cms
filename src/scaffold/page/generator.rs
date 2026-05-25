//! `make page` -- scaffold a filesystem-routed custom admin page.
//!
//! Writes `<config_dir>/templates/pages/<slug>.hbs` (registers route
//! `/admin/p/<slug>` automatically). Prints a copy-pasteable
//! `crap.pages.register("<slug>", { ... })` snippet for `init.lua` so
//! the user can add the sidebar entry without remembering the exact
//! shape -- sidebar registration is optional, the page routes either way.

use std::{fmt::Write as _, fs, path::Path};

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    admin::handlers::shared::paths::custom_page,
    cli,
    scaffold::{
        guards::refuse_file_overwrite, paths, render, to_title_case, validate_template_slug,
    },
};

#[derive(Serialize)]
struct PageCtx<'a> {
    slug: &'a str,
    label: &'a str,
}

/// Options for `make_page`.
pub struct MakePageOptions<'a> {
    pub config_dir: &'a Path,
    pub slug: &'a str,
    /// Sidebar label; defaults to a title-cased version of `slug`.
    pub label: Option<&'a str>,
    /// Sidebar section heading. `None` means "ungrouped at the bottom."
    pub section: Option<&'a str>,
    /// Material Symbols icon name (e.g. `"monitoring"`, `"heart-pulse"`).
    pub icon: Option<&'a str>,
    /// Lua function ref for access control (e.g. `"access.admin_only"`).
    pub access: Option<&'a str>,
    pub force: bool,
}

/// Scaffold the page template.
///
/// # Errors
///
/// Returns an error if the slug is invalid, the file already exists without
/// `--force`, or writing the file fails.
pub fn make_page(opts: &MakePageOptions) -> Result<()> {
    validate_template_slug(opts.slug)?;

    let dir = paths::templates_pages_dir(opts.config_dir);
    fs::create_dir_all(&dir).context("Failed to create templates/pages/ directory")?;

    let file_path = dir.join(format!("{}.hbs", opts.slug));

    refuse_file_overwrite(&file_path, opts.force)?;

    let label = opts
        .label
        .map_or_else(|| to_title_case(opts.slug), str::to_string);

    let hbs = render_page_hbs(opts.slug, &label)?;
    fs::write(&file_path, &hbs)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));
    cli::kv("Route", &custom_page(opts.slug));
    print_register_hint(opts, &label);

    Ok(())
}

fn render_page_hbs(slug: &str, label: &str) -> Result<String> {
    render::render("page", &PageCtx { slug, label })
}

fn print_register_hint(opts: &MakePageOptions, label: &str) {
    let mut snippet = String::new();
    snippet.push_str("\nAdd this to your init.lua so the sidebar shows the entry:\n\n");
    let _ = writeln!(snippet, "  crap.pages.register(\"{}\", {{", opts.slug);
    if let Some(section) = opts.section {
        let _ = writeln!(snippet, "    section = \"{section}\",");
    } else {
        snippet.push_str("    -- section = \"Tools\",  -- optional sidebar group heading\n");
    }
    let _ = writeln!(snippet, "    label = \"{label}\",");
    if let Some(icon) = opts.icon {
        let _ = writeln!(snippet, "    icon = \"{icon}\",");
    } else {
        snippet.push_str("    -- icon = \"monitoring\",  -- Material Symbols icon name\n");
    }
    if let Some(access) = opts.access {
        let _ = writeln!(snippet, "    access = \"{access}\",");
    } else {
        snippet.push_str("    -- access = \"access.admin_only\",  -- optional Lua access fn\n");
    }
    snippet.push_str("  })\n");
    cli::info(&snippet);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_page_template() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_page(&MakePageOptions {
            config_dir: tmp.path(),
            slug: "system_info",
            label: None,
            section: None,
            icon: None,
            access: None,
            force: false,
        })
        .unwrap();
        let file = tmp.path().join("templates/pages/system_info.hbs");
        assert!(file.exists());
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains("layout/base"));
        assert!(body.contains("/admin/p/system_info"));
        // Default label is title-cased slug ("System Info").
        assert!(body.contains("System Info"));
    }

    #[test]
    fn custom_label_used_when_provided() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_page(&MakePageOptions {
            config_dir: tmp.path(),
            slug: "status",
            label: Some("System Status"),
            section: None,
            icon: None,
            access: None,
            force: false,
        })
        .unwrap();
        let body = fs::read_to_string(tmp.path().join("templates/pages/status.hbs")).unwrap();
        assert!(body.contains("System Status"));
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = MakePageOptions {
            config_dir: tmp.path(),
            slug: "x",
            label: None,
            section: None,
            icon: None,
            access: None,
            force: false,
        };
        make_page(&opts).unwrap();
        let err = make_page(&opts).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn rejects_invalid_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = make_page(&MakePageOptions {
            config_dir: tmp.path(),
            slug: "bad slug",
            label: None,
            section: None,
            icon: None,
            access: None,
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("invalid"));
    }

    #[test]
    fn accepts_hyphenated_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_page(&MakePageOptions {
            config_dir: tmp.path(),
            slug: "system-status",
            label: None,
            section: None,
            icon: None,
            access: None,
            force: false,
        })
        .unwrap();
        assert!(
            tmp.path()
                .join("templates/pages/system-status.hbs")
                .exists()
        );
    }
}
