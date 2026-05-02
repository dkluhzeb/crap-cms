//! `make slot` — scaffold a slot-widget HBS file at
//! `<config_dir>/templates/slots/<slot>/<file>.hbs`.
//!
//! Slots are additive — multiple files in the same slot directory render
//! alongside each other in alphabetical order. The scaffold defaults the
//! filename to a sensible widget name when omitted.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli,
    scaffold::{to_title_case, validate_slug},
};

/// Built-in slots and their typical use cases. Used by the scaffold to
/// nudge the user toward the right slot when they pass `--list`.
pub const KNOWN_SLOTS: &[(&str, &str)] = &[
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
pub fn make_slot(opts: &MakeSlotOptions) -> Result<()> {
    validate_slug(opts.slot)?;
    let file = opts.file.unwrap_or("widget");
    validate_slug(file)?;

    let dir = opts
        .config_dir
        .join("templates")
        .join("slots")
        .join(opts.slot);
    fs::create_dir_all(&dir).context("Failed to create slots/<name>/ directory")?;

    let file_path = dir.join(format!("{}.hbs", file));
    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let hbs = render_slot_hbs(opts.slot, file);
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
        "Restart crap-cms (or rely on dev-mode reload) — the slot file renders automatically.",
    );

    Ok(())
}

fn render_slot_hbs(slot: &str, file: &str) -> String {
    let title = to_title_case(file);
    format!(
        r#"{{{{!--
  Slot widget: {title}
  Renders inside the `{slot}` slot, alongside any other contributions.

  Filename order is render order (alphabetical). Prefix with NN- if you
  need to control where this widget appears.

  Page context (`{{{{user}}}}`, `{{{{nav}}}}`, `{{{{crap.site_name}}}}`,
  …) is available here. For dynamic data, register a Lua function via
  `crap.template_data.register("<name>", fn)` and pull it via
  `{{{{data "<name>"}}}}` below.
--}}}}
<div class="card">
  <div class="card__header">
    <h3>{title}</h3>
  </div>
  <div class="card__body">
    <p>Slot widget. Edit
      <code>templates/slots/{slot}/{file}.hbs</code> in your config dir.</p>

    {{{{!-- {{{{#with (data "{file}_data")}}}}    --}}}}
    {{{{!--   <p>{{{{this.value}}}}</p>          --}}}}
    {{{{!-- {{{{/with}}}}                         --}}}}
  </div>
</div>
"#,
        slot = slot,
        file = file,
        title = title,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
