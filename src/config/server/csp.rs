//! Content-Security-Policy header configuration for the admin UI.

use serde::{Deserialize, Serialize};

/// Content-Security-Policy configuration for the admin UI.
///
/// Each field is a list of sources for the corresponding CSP directive.
/// Theme developers can extend these lists to allow external resources
/// (CDNs, fonts, analytics, etc.).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CspConfig {
    /// Enable CSP header. Default: true. Set to false to disable entirely.
    pub enabled: bool,
    /// `default-src` directive -- fallback for unspecified directives.
    pub default_src: Vec<String>,
    /// `script-src` directive -- allowed script sources.
    pub script_src: Vec<String>,
    /// `style-src` directive -- allowed stylesheet sources.
    pub style_src: Vec<String>,
    /// `font-src` directive -- allowed font sources.
    pub font_src: Vec<String>,
    /// `img-src` directive -- allowed image sources.
    pub img_src: Vec<String>,
    /// `connect-src` directive -- allowed fetch/XHR/WebSocket targets.
    pub connect_src: Vec<String>,
    /// `frame-ancestors` directive -- who can embed this page. Replaces X-Frame-Options.
    pub frame_ancestors: Vec<String>,
    /// `form-action` directive -- allowed form submission targets.
    pub form_action: Vec<String>,
    /// `base-uri` directive -- allowed `<base>` tag URLs.
    pub base_uri: Vec<String>,
}

impl Default for CspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_src: vec!["'self'".into()],
            // `'unsafe-inline'` is intentionally absent -- inline `<script>` tags
            // are allowed only via the per-request nonce (see `build_header_value`).
            // Built-in and overlay templates must mark inline scripts with
            // `nonce="{{crap.csp_nonce}}"`. Only `'self'` is allowed because
            // every script the built-in admin loads is served from the same
            // origin: `/static/vendor/htmx.js` (vendored -- see
            // `scripts/bundle-htmx.sh`), `/static/vendor/codemirror.js`,
            // `/static/vendor/prosemirror.js`, and the ES-module entry
            // `/static/components/index.js`.
            script_src: vec!["'self'".into()],
            // `'unsafe-inline'` is intentionally absent. The built-in admin
            // is fully nonce/CSP-friendly:
            //   - Web Components use constructable stylesheets
            //     (`new CSSStyleSheet()` + `adoptedStyleSheets`) which are
            //     CSP-exempt by spec.
            //   - Templates use the `hidden` attribute / classes / data-attr
            //     selectors instead of `style="..."`.
            //   - HTMX's only inline style is the indicator-styles injection,
            //     disabled via `<meta name="htmx-config" content='{"includeIndicatorStyles":false}'>`
            //     in `base.hbs`; the equivalent CSS rules ship in
            //     `static/styles/base/reset.css`.
            //   - Programmatic `element.style.foo = ...` is exempt from CSP
            //     and remains available for runtime layout.
            style_src: vec!["'self'".into(), "https://fonts.googleapis.com".into()],
            font_src: vec!["'self'".into(), "https://fonts.gstatic.com".into()],
            img_src: vec!["'self'".into(), "data:".into()],
            connect_src: vec!["'self'".into()],
            frame_ancestors: vec!["'none'".into()],
            form_action: vec!["'self'".into()],
            base_uri: vec!["'self'".into()],
        }
    }
}

impl CspConfig {
    /// Build the CSP header value string from configured directives.
    ///
    /// When `nonce` is `Some`, `'nonce-XYZ'` is appended to `script-src` so
    /// inline `<script nonce="XYZ">` tags are allowed. The same nonce must be
    /// embedded in the rendered HTML (see `crap.csp_nonce` in the template
    /// context). When `nonce` is `None`, no nonce directive is added -- any
    /// inline script in the response will be blocked unless the user has
    /// explicitly configured `'unsafe-inline'` in their overrides.
    ///
    /// Returns `None` if CSP is disabled entirely (`enabled = false`).
    pub fn build_header_value(&self, nonce: Option<&str>) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let script_src_with_nonce: Vec<String>;
        let script_src: &[String] = match nonce {
            Some(n) => {
                script_src_with_nonce = self
                    .script_src
                    .iter()
                    .cloned()
                    .chain(std::iter::once(format!("'nonce-{n}'")))
                    .collect();

                &script_src_with_nonce
            }
            None => &self.script_src,
        };

        let mut directives = Vec::new();

        let pairs: &[(&str, &[String])] = &[
            ("default-src", &self.default_src),
            ("script-src", script_src),
            ("style-src", &self.style_src),
            ("font-src", &self.font_src),
            ("img-src", &self.img_src),
            ("connect-src", &self.connect_src),
            ("frame-ancestors", &self.frame_ancestors),
            ("form-action", &self.form_action),
            ("base-uri", &self.base_uri),
        ];

        for (name, sources) in pairs {
            if !sources.is_empty() {
                directives.push(format!("{} {}", name, sources.join(" ")));
            }
        }

        if directives.is_empty() {
            return None;
        }

        Some(directives.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::CrapConfig;

    use super::*;

    #[test]
    fn csp_config_defaults_produce_valid_header() {
        let csp = CspConfig::default();
        let header = csp.build_header_value(None);
        assert!(header.is_some());
        let h = header.unwrap();
        assert!(h.contains("default-src 'self'"));
        // Defaults no longer permit `'unsafe-inline'` for scripts -- the nonce
        // mechanism replaces it.
        assert!(h.contains("script-src 'self'"));
        assert!(
            !h.contains("https://unpkg.com"),
            "no third-party CDN; htmx is now vendored at /static/vendor/htmx.js"
        );
        assert!(!h.contains("script-src 'self' 'unsafe-inline'"));
        // `style-src` is also free of `'unsafe-inline'` -- HTMX's runtime style
        // injection is disabled via `htmx.config.includeIndicatorStyles=false`
        // (set in the layout meta tag), and the indicator CSS ships in
        // `static/styles.css` instead.
        assert!(h.contains("style-src 'self' https://fonts.googleapis.com"));
        assert!(!h.contains("style-src 'self' 'unsafe-inline'"));
        assert!(h.contains("font-src 'self' https://fonts.gstatic.com"));
        assert!(h.contains("img-src 'self' data:"));
        assert!(h.contains("connect-src 'self'"));
        assert!(h.contains("frame-ancestors 'none'"));
        assert!(h.contains("form-action 'self'"));
        assert!(h.contains("base-uri 'self'"));
    }

    #[test]
    fn csp_config_with_nonce_appends_nonce_to_script_src() {
        let csp = CspConfig::default();
        let header = csp.build_header_value(Some("abc123")).unwrap();
        assert!(header.contains("script-src 'self' 'nonce-abc123'"));
        // Nonce is scoped to scripts only -- style-src does not get a nonce.
        assert!(!header.contains("style-src 'self' https://fonts.googleapis.com 'nonce-"));
    }

    #[test]
    fn csp_config_without_nonce_omits_nonce_directive() {
        let csp = CspConfig::default();
        let header = csp.build_header_value(None).unwrap();
        assert!(!header.contains("'nonce-"));
    }

    #[test]
    fn csp_config_disabled_returns_none() {
        let csp = CspConfig {
            enabled: false,
            ..CspConfig::default()
        };
        assert!(csp.build_header_value(None).is_none());
        assert!(csp.build_header_value(Some("abc")).is_none());
    }

    #[test]
    fn csp_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin.csp]\nenabled = true\nscript_src = [\"'self'\", \"https://cdn.example.com\"]\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.csp.enabled);
        assert_eq!(
            config.admin.csp.script_src,
            vec!["'self'", "https://cdn.example.com"]
        );
        // Other directives keep defaults
        assert!(config.admin.csp.style_src.contains(&"'self'".to_string()));
    }

    #[test]
    fn csp_config_disabled_via_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin.csp]\nenabled = false\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(!config.admin.csp.enabled);
        assert!(config.admin.csp.build_header_value(None).is_none());
    }

    #[test]
    fn csp_config_empty_directive_omitted() {
        let csp = CspConfig {
            enabled: true,
            default_src: vec!["'self'".into()],
            script_src: vec![],
            style_src: vec![],
            font_src: vec![],
            img_src: vec![],
            connect_src: vec![],
            frame_ancestors: vec![],
            form_action: vec![],
            base_uri: vec![],
        };
        let header = csp.build_header_value(None).unwrap();
        assert_eq!(header, "default-src 'self'");
        assert!(!header.contains("script-src"));
    }
}
