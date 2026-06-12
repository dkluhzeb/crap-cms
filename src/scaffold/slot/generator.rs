//! `make slot` -- scaffold a slot-widget HBS file at
//! `<config_dir>/templates/slots/<slot>/<file>.hbs`.
//!
//! Slots are additive -- multiple files in the same slot directory render
//! alongside each other in alphabetical order. The scaffold defaults the
//! filename to a sensible widget name when omitted.

use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    cli,
    scaffold::{
        guards::refuse_file_overwrite, paths, render, to_title_case, validate_template_slug,
    },
};

#[derive(Serialize)]
struct SlotCtx<'a> {
    slot: &'a str,
    file: &'a str,
    title: String,
}

/// Built-in slots and their typical use cases. Used by the scaffold to
/// nudge the user toward the right slot when they pass `--list`.
const KNOWN_SLOTS: &[(&str, &str)] = &[
    (
        "head_extras",
        "extra <head> tags (OG, robots, PWA, analytics)",
    ),
    (
        "body_end_scripts",
        "end-of-body analytics / event listeners",
    ),
    ("page_header_actions", "extra buttons in the top header bar"),
    ("dashboard_widgets", "custom dashboard cards"),
    (
        "collection_edit_toolbar",
        "extra toolbar actions on collection edit pages",
    ),
    (
        "collection_edit_sidebar",
        "extra sidebar panels on collection edit pages",
    ),
    (
        "sidebar_bottom",
        "extra navigation links pinned to the bottom of the left sidebar",
    ),
    ("login_extras", "additional content on the login page"),
];

/// Options for `make_slot`.
pub struct MakeSlotOptions<'a> {
    pub config_dir: &'a Path,
    pub slot: &'a str,
    /// Filename inside the slot directory (without `.hbs`). Defaults to
    /// `widget` when omitted. Filename order controls render order.
    pub file: Option<&'a str>,
    pub force: bool,
}

/// Scaffold the slot widget HBS file.
///
/// # Errors
///
/// Returns an error if the slot/file slug is invalid, the file already
/// exists without `--force`, or writing fails.
pub fn make_slot(opts: &MakeSlotOptions) -> Result<()> {
    validate_template_slug(opts.slot)?;
    let file = opts.file.unwrap_or("widget");
    validate_template_slug(file)?;

    let dir = paths::templates_slot_dir(opts.config_dir, opts.slot);
    fs::create_dir_all(&dir).context("Failed to create slots/<name>/ directory")?;

    let file_path = dir.join(format!("{file}.hbs"));
    refuse_file_overwrite(&file_path, opts.force)?;

    let hbs = render_slot_hbs(opts.slot, file)?;
    fs::write(&file_path, &hbs)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));
    if !KNOWN_SLOTS.iter().any(|(s, _)| *s == opts.slot) {
        cli::warning(&format!(
            "Slot `{}` is not one of the built-in slots. Verify the slot is declared somewhere via {{{{slot \"{}\"}}}}, or you'll see no output.",
            opts.slot, opts.slot,
        ));
    }
    cli::info(
        "Restart crap-cms (or rely on dev-mode reload) -- the slot file renders automatically.",
    );

    Ok(())
}

fn render_slot_hbs(slot: &str, file: &str) -> Result<String> {
    render::render(
        "slot",
        &SlotCtx {
            slot,
            file,
            title: to_title_case(file),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scaffolded widget must already satisfy `crap-cms fmt` —
    /// otherwise a fresh `make slot` immediately fails the user's
    /// pre-commit `fmt --check`.
    #[test]
    fn generated_template_is_formatter_clean() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_slot(&MakeSlotOptions {
            config_dir: tmp.path(),
            slot: "dashboard_widgets",
            file: None,
            force: false,
        })
        .unwrap();

        let path = tmp
            .path()
            .join("templates/slots/dashboard_widgets/widget.hbs");
        let src = fs::read_to_string(path).unwrap();
        let formatted = crate::fmt::format(&src).unwrap();
        assert_eq!(formatted, src, "make slot output must be fmt-clean");
    }

    #[test]
    fn writes_slot_widget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_slot(&MakeSlotOptions {
            config_dir: tmp.path(),
            slot: "dashboard_widgets",
            file: Some("weather"),
            force: false,
        })
        .unwrap();
        let file = tmp
            .path()
            .join("templates/slots/dashboard_widgets/weather.hbs");
        assert!(file.exists());
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains("Weather"));
        assert!(body.contains("dashboard_widgets/weather.hbs"));
    }

    #[test]
    fn defaults_filename_to_widget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_slot(&MakeSlotOptions {
            config_dir: tmp.path(),
            slot: "page_header_actions",
            file: None,
            force: false,
        })
        .unwrap();
        let file = tmp
            .path()
            .join("templates/slots/page_header_actions/widget.hbs");
        assert!(file.exists());
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = MakeSlotOptions {
            config_dir: tmp.path(),
            slot: "dashboard_widgets",
            file: Some("x"),
            force: false,
        };
        make_slot(&opts).unwrap();
        let err = make_slot(&opts).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn accepts_hyphenated_slot_and_file_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_slot(&MakeSlotOptions {
            config_dir: tmp.path(),
            slot: "dashboard-widgets",
            file: Some("status-card"),
            force: false,
        })
        .unwrap();
        assert!(
            tmp.path()
                .join("templates/slots/dashboard-widgets/status-card.hbs")
                .exists()
        );
    }
}
