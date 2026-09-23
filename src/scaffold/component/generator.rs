//! `make component` -- scaffold a custom Web Component JS file at
//! `<config_dir>/static/components/<tag>.js`. Prints the one-line
//! `import './<tag>.js';` to add to `custom.js` for registration.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use include_dir::Dir;
use serde::Serialize;

use crate::{
    cli,
    scaffold::{EMBEDDED_STATIC, guards::refuse_file_overwrite, paths, render},
};

#[derive(Serialize)]
struct ComponentCtx<'a> {
    tag: &'a str,
    class_name: String,
}

/// Options for `make_component`.
pub struct MakeComponentOptions<'a> {
    pub config_dir: &'a Path,
    /// Tag name. Must contain a `-` (HTML custom-element requirement).
    pub tag: &'a str,
    pub force: bool,
}

/// Scaffold the JS file.
///
/// # Errors
///
/// Returns an error if the tag is invalid, the file already exists without
/// `--force`, or writing the file fails.
pub fn make_component(opts: &MakeComponentOptions) -> Result<()> {
    let file_path = component_path(opts.config_dir, opts.tag)?;
    refuse_file_overwrite(&file_path, opts.force)?;

    let dir = paths::static_components_dir(opts.config_dir);
    fs::create_dir_all(&dir).context("Failed to create static/components/ directory")?;

    let js = render_component_js(opts.tag)?;
    fs::write(&file_path, &js)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));
    cli::info(&format!(
        "Add this line to <config_dir>/static/components/custom.js so the admin loads it:\n\n  import './{}.js';",
        opts.tag,
    ));

    Ok(())
}

/// Where the component for `tag` is written, once `tag` is known to be a
/// valid custom-element name. Lets a generator that also writes a component
/// refuse before it writes anything.
///
/// # Errors
///
/// Returns an error if `tag` isn't a valid custom-element name, or names a
/// built-in component.
pub(crate) fn component_path(config_dir: &Path, tag: &str) -> Result<PathBuf> {
    validate_tag(tag)?;
    refuse_builtin(tag)?;

    Ok(paths::static_components_dir(config_dir).join(format!("{tag}.js")))
}

/// HTML custom-element tag rule: must contain `-`, must start with
/// ASCII lowercase letter, must be all lowercase + alphanumeric +
/// `-` thereafter. We tighten further: no leading/trailing dash, no
/// `--`.
fn validate_tag(tag: &str) -> Result<()> {
    if tag.is_empty() {
        bail!("tag must not be empty");
    }
    if !tag.contains('-') {
        bail!("tag '{tag}' is invalid -- custom elements must contain a hyphen (e.g. 'my-widget')");
    }
    if tag.starts_with('-') || tag.ends_with('-') {
        bail!("tag '{tag}' must not start or end with a hyphen");
    }
    if tag.contains("--") {
        bail!("tag '{tag}' must not contain consecutive hyphens");
    }
    if !tag.chars().next().unwrap().is_ascii_lowercase() {
        bail!("tag '{tag}' must start with an ASCII lowercase letter");
    }
    for c in tag.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if !ok {
            bail!(
                "tag '{tag}' contains invalid character '{c}' (lowercase letters / digits / `-` only)"
            );
        }
    }
    Ok(())
}

/// Refuse a tag that would shadow a built-in component: its module file
/// (`static/components/<tag>.js` overlays the built-in module of that name,
/// so a scaffold there replaces it) or its element name (a second
/// `customElements.define` of a built-in tag throws in the browser).
fn refuse_builtin(tag: &str) -> Result<()> {
    if EMBEDDED_STATIC
        .get_file(format!("components/{tag}.js"))
        .is_some()
    {
        bail!(
            "'{tag}' is a built-in component module -- a component written there would \
             replace it. To customize the built-in, run \
             `crap-cms templates extract components/{tag}.js`; otherwise pick another tag"
        );
    }

    if builtin_element_tags().iter().any(|t| t == tag) {
        bail!("'{tag}' is a built-in element name -- pick another tag");
    }

    Ok(())
}

/// Every element name a built-in component registers with
/// `customElements.define`.
fn builtin_element_tags() -> Vec<String> {
    let mut tags = Vec::new();

    if let Some(dir) = EMBEDDED_STATIC.get_dir("components") {
        collect_defined_tags(dir, &mut tags);
    }

    tags
}

fn collect_defined_tags(dir: &Dir<'_>, out: &mut Vec<String>) {
    for file in dir.files() {
        if let Some(src) = file.contents_utf8() {
            out.extend(defined_tags(src));
        }
    }

    for sub in dir.dirs() {
        collect_defined_tags(sub, out);
    }
}

/// The tags a script passes to `customElements.define`.
fn defined_tags(src: &str) -> impl Iterator<Item = String> + '_ {
    src.split("customElements.define(")
        .skip(1)
        .filter_map(|rest| {
            let rest = rest.trim_start();
            let quote = rest
                .chars()
                .next()
                .filter(|c| matches!(c, '\'' | '"' | '`'))?;

            rest[1..].split(quote).next().map(str::to_string)
        })
}

fn render_component_js(tag: &str) -> Result<String> {
    render::render(
        "component",
        &ComponentCtx {
            tag,
            class_name: tag_to_class_name(tag),
        },
    )
}

/// Convert `my-widget` -> `MyWidget`.
fn tag_to_class_name(tag: &str) -> String {
    tag.split('-')
        .map(|s| {
            let mut chars = s.chars();
            chars.next().map_or(String::new(), |c| {
                c.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_component_js() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_component(&MakeComponentOptions {
            config_dir: tmp.path(),
            tag: "my-widget",
            force: false,
        })
        .unwrap();
        let file = tmp.path().join("static/components/my-widget.js");
        assert!(file.exists());
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains("class MyWidget extends HTMLElement"));
        assert!(body.contains("customElements.define('my-widget'"));
        assert!(body.contains("static formAssociated = true"));
    }

    /// Regression: the previous scaffold shipped `shadowRoot.innerHTML = ...`
    /// with an inline `<style>` block -- the exact pattern that alpha.8
    /// migrated all built-in components away from. Lock the scaffold in
    /// to the new constructable-stylesheet + `h()` pattern so new
    /// components don't drift back.
    #[test]
    fn uses_constructable_stylesheets_and_h_helper() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_component(&MakeComponentOptions {
            config_dir: tmp.path(),
            tag: "my-widget",
            force: false,
        })
        .unwrap();
        let file = tmp.path().join("static/components/my-widget.js");
        let body = fs::read_to_string(&file).unwrap();

        assert!(body.contains("import { css } from './_internal/css.js'"));
        assert!(body.contains("import { h } from './_internal/h.js'"));
        assert!(body.contains("adoptedStyleSheets = [sheet]"));
        assert!(body.contains("h('div'"));

        // No inline-style or innerHTML pattern.
        assert!(!body.contains("innerHTML"));
        assert!(!body.contains("<style>"));
    }

    #[test]
    fn rejects_tag_without_hyphen() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = make_component(&MakeComponentOptions {
            config_dir: tmp.path(),
            tag: "rating",
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("hyphen"));
    }

    #[test]
    fn rejects_uppercase_tag() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = make_component(&MakeComponentOptions {
            config_dir: tmp.path(),
            tag: "My-Widget",
            force: false,
        })
        .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("invalid character")
                || err.to_string().contains("lowercase")
        );
    }

    /// Regression: `make component dirty-form` wrote a skeleton over the
    /// built-in module's overlay path, silently replacing the built-in.
    #[test]
    fn rejects_a_built_in_module_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = make_component(&MakeComponentOptions {
            config_dir: tmp.path(),
            tag: "dirty-form",
            force: false,
        })
        .unwrap_err()
        .to_string();

        assert!(err.contains("built-in component module"), "{err}");
        assert!(!tmp.path().join("static/components/dirty-form.js").exists());
    }

    #[test]
    fn rejects_a_built_in_element_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = make_component(&MakeComponentOptions {
            config_dir: tmp.path(),
            tag: "crap-tags",
            force: false,
        })
        .unwrap_err()
        .to_string();

        assert!(err.contains("built-in element"), "{err}");
    }

    #[test]
    fn defined_tags_reads_every_quote_style() {
        let src = "customElements.define('crap-a', A);\ncustomElements.define(\n  \"crap-b\", B);";

        assert_eq!(
            defined_tags(src).collect::<Vec<_>>(),
            vec!["crap-a", "crap-b"]
        );
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = MakeComponentOptions {
            config_dir: tmp.path(),
            tag: "my-widget",
            force: false,
        };
        make_component(&opts).unwrap();
        let err = make_component(&opts).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}
