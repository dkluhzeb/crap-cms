//! `make component` — scaffold a custom Web Component JS file at
//! `<config_dir>/static/components/<tag>.js`. Prints the one-line
//! `import './<tag>.js';` to add to `custom.js` for registration.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};

use crate::cli;

/// Options for `make_component`.
pub struct MakeComponentOptions<'a> {
    pub config_dir: &'a Path,
    /// Tag name. Must contain a `-` (HTML custom-element requirement).
    pub tag: &'a str,
    pub force: bool,
}

/// Scaffold the JS file.
pub fn make_component(opts: &MakeComponentOptions) -> Result<()> {
    validate_tag(opts.tag)?;

    let dir = opts.config_dir.join("static").join("components");
    fs::create_dir_all(&dir).context("Failed to create static/components/ directory")?;

    let file_path = dir.join(format!("{}.js", opts.tag));
    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let js = render_component_js(opts.tag);
    fs::write(&file_path, &js)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));
    cli::info(&format!(
        "Add this line to <config_dir>/static/components/custom.js so the admin loads it:\n\n  import './{}.js';",
        opts.tag,
    ));

    Ok(())
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
        bail!(
            "tag '{}' is invalid — custom elements must contain a hyphen (e.g. 'my-widget')",
            tag
        );
    }
    if tag.starts_with('-') || tag.ends_with('-') {
        bail!("tag '{}' must not start or end with a hyphen", tag);
    }
    if tag.contains("--") {
        bail!("tag '{}' must not contain consecutive hyphens", tag);
    }
    if !tag.chars().next().unwrap().is_ascii_lowercase() {
        bail!("tag '{}' must start with an ASCII lowercase letter", tag);
    }
    for c in tag.chars() {
        let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if !ok {
            bail!(
                "tag '{}' contains invalid character '{}' (lowercase letters / digits / `-` only)",
                tag,
                c
            );
        }
    }
    Ok(())
}

fn render_component_js(tag: &str) -> String {
    let class_name = tag_to_class_name(tag);
    format!(
        r#"/**
 * <{tag}> — custom Web Component.
 *
 * Registered via `<config_dir>/static/components/custom.js` (which
 * `index.js` auto-imports). Form-associated by default so the value
 * participates in native form submission and validation; remove
 * `static formAssociated = true` and the `setFormValue` call if you
 * don't need form integration.
 *
 * @module {tag}
 * @stability stable
 */

class {class_name} extends HTMLElement {{
  static formAssociated = true;
  static observedAttributes = ['value'];

  constructor() {{
    super();
    /** @type {{ElementInternals}} */
    this._internals = this.attachInternals();
    this.attachShadow({{ mode: 'open' }});
  }}

  connectedCallback() {{
    this._render();
  }}

  attributeChangedCallback(name, _old, value) {{
    if (name === 'value') {{
      this._internals.setFormValue(value ?? '');
      this._render();
    }}
  }}

  /** Public getter/setter for `.value`, mirrored to the attribute. */
  get value() {{
    return this.getAttribute('value') ?? '';
  }}
  set value(v) {{
    this.setAttribute('value', v);
  }}

  _render() {{
    if (!this.shadowRoot) return;
    const v = this.value;
    this.shadowRoot.innerHTML = `
      <style>
        :host {{ display: inline-block; }}
        .container {{ padding: var(--space-sm, 0.5rem); }}
      </style>
      <div class="container" part="container">
        <span part="value">${{v || '(empty)'}}</span>
      </div>
    `;
  }}

  /** Dispatch `crap:change` so <crap-dirty-form> picks up edits. */
  _emitChange() {{
    this.dispatchEvent(new Event('crap:change', {{ bubbles: true, composed: true }}));
  }}
}}

if (!customElements.get('{tag}')) {{
  customElements.define('{tag}', {class_name});
}}
"#,
        tag = tag,
        class_name = class_name,
    )
}

/// Convert `my-widget` → `MyWidget`.
fn tag_to_class_name(tag: &str) -> String {
    tag.split('-')
        .map(|s| {
            let mut chars = s.chars();
            chars.next().map_or(String::new(), |c| {
                c.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join("")
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
