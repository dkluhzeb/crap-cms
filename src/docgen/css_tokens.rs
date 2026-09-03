//! Generate `docs/src/admin-ui/reference/css-variables.md` from
//! `static/styles/tokens.css` — the tokens file IS the contract, so the
//! reference is rendered from it instead of hand-maintaining a twin.

use std::fmt::Write as _;

use crate::scaffold::EMBEDDED_STATIC;

/// Render the full css-variables reference page.
///
/// # Panics
///
/// Panics when the embedded `styles/tokens.css` is missing or not UTF-8 —
/// a build-inconsistency, caught by the docgen test.
#[must_use]
pub fn generate_css_variables_md() -> String {
    let css = EMBEDDED_STATIC
        .get_file("styles/tokens.css")
        .expect("styles/tokens.css embedded")
        .contents_utf8()
        .expect("tokens.css is UTF-8");

    let mut out = String::from(
        "<!-- GENERATED FILE — do not edit. Regenerate with `cargo xtask gen-doc-tables`. -->\n\n\
         # CSS Variables\n\n\
         The admin UI uses CSS custom properties for every design decision —\n\
         spacing, color, typography, sizes, shadows, transitions, and\n\
         component-specific knobs. Themes override these on\n\
         `html[data-theme=\"…\"]`, so any component reading the variables\n\
         automatically participates in theming.\n\n\
         This reference is generated from\n\
         [`static/styles/tokens.css`](https://github.com/dkluhs/crap-cms/blob/main/static/styles/tokens.css)\n\
         — the tokens file is the contract. Every token below is stable\n\
         theming surface; sizes derive from `--base` with small multipliers,\n\
         so changing `--base` rescales the whole admin proportionally.\n\n",
    );

    let mut in_root = false;
    let mut table_open = false;

    for line in css.lines() {
        let t = line.trim();

        if t.starts_with(":root") {
            in_root = true;
            continue;
        }
        if !in_root {
            continue;
        }
        if t == "}" {
            break;
        }

        if let Some(comment) = t.strip_prefix("/*") {
            // A group comment opens a new section. Multi-line comments keep
            // only their first line as the heading.
            let heading = comment.trim_end_matches("*/").trim();
            if heading.is_empty() {
                continue;
            }
            if table_open {
                out.push('\n');
                table_open = false;
            }
            let _ = write!(out, "## {heading}\n\n");
            continue;
        }

        if let Some(rest) = t.strip_prefix("--") {
            let Some((name, value)) = rest.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_end_matches(';');
            // Inline `/* … */` trailing comments become the notes column.
            let (value, note) = match value.split_once("/*") {
                Some((v, n)) => (v.trim(), n.trim_end_matches("*/").trim()),
                None => (value, ""),
            };
            if !table_open {
                out.push_str("| Token | Value | Notes |\n|---|---|---|\n");
                table_open = true;
            }
            let _ = writeln!(out, "| `--{name}` | `{value}` | {note} |");
        }
    }

    out.push_str(
        "\nTokens are declared under `:root` with `color-scheme: light`;\n\
         dark values live in `static/styles/themes/default.css` under\n\
         `html[data-theme=\"dark\"]`. Adding a token is a public surface\n\
         change — document intent here by keeping the group comment in\n\
         `tokens.css` accurate, and run `crap-cms theme validate` when a\n\
         contrast pair changes.\n",
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_sections_and_tokens() {
        let md = generate_css_variables_md();
        assert!(md.contains("## Colors - Primary"));
        assert!(md.contains("| `--color-primary` |"));
        assert!(md.contains("## Spacing (base × n)"));
        assert!(md.contains("| `--space-sm` |"));
    }
}
