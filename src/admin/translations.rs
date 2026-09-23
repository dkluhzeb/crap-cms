//! Admin UI translation loading: compiled-in English + German, config dir overlay.

use std::{collections::HashMap, fs, path::Path};

use tracing::warn;

static DEFAULT_EN: &str = include_str!("../../translations/en.json");
static DEFAULT_DE: &str = include_str!("../../translations/de.json");

/// Holds resolved translation strings for all locales.
pub struct Translations {
    locales: HashMap<String, HashMap<String, String>>,
}

impl Translations {
    /// Load translations: compiled-in locales as base, overlaid with
    /// `<config_dir>/translations/*.json` files if they exist.
    ///
    /// Every overlay file names a UI locale by its stem: `de.json` extends
    /// the built-in German, `fr.json` adds French to the locale picker. A
    /// file that cannot be read or is not a flat string map is skipped with a
    /// warning naming the file and the reason — never silently.
    #[must_use]
    pub fn load(config_dir: &Path) -> Self {
        let mut locales: HashMap<String, HashMap<String, String>> = HashMap::new();

        // Load compiled-in defaults
        if let Ok(en) = serde_json::from_str::<HashMap<String, String>>(DEFAULT_EN) {
            locales.insert("en".to_string(), en);
        }

        if let Ok(de) = serde_json::from_str::<HashMap<String, String>>(DEFAULT_DE) {
            locales.insert("de".to_string(), de);
        }

        apply_overlays(&config_dir.join("translations"), &mut locales);

        Translations { locales }
    }

    /// Get a translated string by locale and key.
    /// Falls back to "en" locale, then to the key itself.
    #[must_use]
    pub fn get<'a>(&'a self, locale: &str, key: &'a str) -> &'a str {
        // Try requested locale
        if let Some(strings) = self.locales.get(locale)
            && let Some(val) = strings.get(key)
        {
            return val.as_str();
        }

        // Fallback to English
        if locale != "en"
            && let Some(strings) = self.locales.get("en")
            && let Some(val) = strings.get(key)
        {
            return val.as_str();
        }

        // Return key itself
        key
    }

    /// Get a translated string and interpolate `{{var}}` placeholders with the given params.
    #[must_use]
    pub fn get_interpolated(
        &self,
        locale: &str,
        key: &str,
        params: &HashMap<String, String>,
    ) -> String {
        let template = self.get(locale, key);

        if params.is_empty() {
            return template.to_string();
        }

        // Single pass over the template: each `{{key}}` is replaced from `params`
        // and the substituted text is never re-scanned. A previous `replace`
        // loop over the params map was order-dependent (HashMap iteration) and
        // could substitute a placeholder that appeared *inside* an already-
        // inserted value. Unknown placeholders are kept literally.
        let mut result = String::with_capacity(template.len());
        let mut rest = template;

        while let Some(start) = rest.find("{{") {
            result.push_str(&rest[..start]);
            let after = &rest[start + 2..];

            let Some(end) = after.find("}}") else {
                // No closing braces — copy the remainder verbatim and stop.
                result.push_str(&rest[start..]);
                return result;
            };

            let name = &after[..end];
            if let Some(value) = params.get(name) {
                result.push_str(value);
            } else {
                result.push_str("{{");
                result.push_str(name);
                result.push_str("}}");
            }

            rest = &after[end + 2..];
        }

        result.push_str(rest);
        result
    }

    /// Return the list of available locale codes.
    #[must_use]
    pub fn available_locales(&self) -> Vec<&str> {
        let mut locales: Vec<&str> = self
            .locales
            .keys()
            .map(std::string::String::as_str)
            .collect();

        locales.sort_unstable();

        locales
    }
}

/// The UI locale an overlay file defines: the stem of a `*.json` file.
fn overlay_locale(path: &Path) -> Option<&str> {
    if path.extension().is_none_or(|ext| ext != "json") {
        return None;
    }

    path.file_stem().and_then(|s| s.to_str())
}

/// Read one overlay file as a flat `key → string` map, or say why not.
fn read_overlay(path: &Path) -> Result<HashMap<String, String>, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;

    serde_json::from_str(&content).map_err(|e| format!("not a flat string map: {e}"))
}

/// Overlay every `*.json` file in `dir` onto `locales`, warning about each
/// file that cannot be used.
fn apply_overlays(dir: &Path, locales: &mut HashMap<String, HashMap<String, String>>) {
    if !dir.exists() {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "Cannot read admin translations directory {}: {e}",
                dir.display()
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(locale) = overlay_locale(&path) else {
            continue;
        };

        match read_overlay(&path) {
            Ok(overrides) => locales
                .entry(locale.to_string())
                .or_default()
                .extend(overrides),
            Err(reason) => warn!(
                "Skipping admin translation file {}: {reason}",
                path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_multiple_locales() {
        let t = Translations::load(Path::new("/nonexistent"));
        assert!(t.locales.contains_key("en"));
        assert!(t.locales.contains_key("de"));
    }

    #[test]
    fn get_english_key() {
        let t = Translations::load(Path::new("/nonexistent"));
        assert_eq!(t.get("en", "save"), "Save");
    }

    #[test]
    fn get_german_key() {
        let t = Translations::load(Path::new("/nonexistent"));
        assert_eq!(t.get("de", "save"), "Speichern");
    }

    #[test]
    fn get_fallback_to_english() {
        let t = Translations::load(Path::new("/nonexistent"));
        // Use a key that only exists in en (if de is missing it)
        // Actually both should have all keys, so test with unknown locale
        assert_eq!(t.get("fr", "save"), "Save");
    }

    #[test]
    fn get_missing_key_returns_key() {
        let t = Translations::load(Path::new("/nonexistent"));
        assert_eq!(
            t.get("en", "nonexistent_key_12345"),
            "nonexistent_key_12345"
        );
    }

    fn translations_with(key: &str, template: &str) -> Translations {
        let mut inner = HashMap::new();
        inner.insert(key.to_string(), template.to_string());
        let mut locales = HashMap::new();
        locales.insert("en".to_string(), inner);
        Translations { locales }
    }

    /// Regression: interpolation is single-pass. A param value that itself
    /// looks like a `{{placeholder}}` must not be re-substituted, and the
    /// result must not depend on `HashMap` iteration order.
    #[test]
    fn interpolation_is_single_pass() {
        let t = translations_with("greet", "{{a}} and {{b}}");
        let mut params = HashMap::new();
        params.insert("a".to_string(), "{{b}}".to_string());
        params.insert("b".to_string(), "X".to_string());
        assert_eq!(t.get_interpolated("en", "greet", &params), "{{b}} and X");
    }

    #[test]
    fn interpolation_keeps_unknown_placeholder() {
        let t = translations_with("msg", "hi {{name}} {{unknown}}");
        let mut params = HashMap::new();
        params.insert("name".to_string(), "Sam".to_string());
        assert_eq!(
            t.get_interpolated("en", "msg", &params),
            "hi Sam {{unknown}}"
        );
    }

    #[test]
    fn interpolation_handles_unclosed_placeholder() {
        let t = translations_with("msg", "value {{oops");
        let mut params = HashMap::new();
        params.insert("oops".to_string(), "X".to_string());
        // No closing braces — the remainder is kept verbatim.
        assert_eq!(t.get_interpolated("en", "msg", &params), "value {{oops");
    }

    #[test]
    fn get_interpolated_replaces_vars() {
        let mut locales = HashMap::new();
        let mut en = HashMap::new();
        en.insert(
            "greeting".to_string(),
            "Hello {{name}}, welcome to {{place}}!".to_string(),
        );
        locales.insert("en".to_string(), en);
        let t = Translations { locales };

        let mut params = HashMap::new();
        params.insert("name".to_string(), "Alice".to_string());
        params.insert("place".to_string(), "CMS".to_string());
        let result = t.get_interpolated("en", "greeting", &params);
        assert_eq!(result, "Hello Alice, welcome to CMS!");
    }

    #[test]
    fn get_interpolated_no_params() {
        let mut locales = HashMap::new();
        let mut en = HashMap::new();
        en.insert("plain".to_string(), "No vars here".to_string());
        locales.insert("en".to_string(), en);
        let t = Translations { locales };
        let result = t.get_interpolated("en", "plain", &HashMap::new());
        assert_eq!(result, "No vars here");
    }

    #[test]
    fn get_interpolated_missing_key_returns_key() {
        let t = Translations {
            locales: HashMap::new(),
        };
        let result = t.get_interpolated("en", "missing", &HashMap::new());
        assert_eq!(result, "missing");
    }

    #[test]
    fn available_locales_sorted() {
        let t = Translations::load(Path::new("/nonexistent"));
        let locales = t.available_locales();
        assert!(locales.contains(&"en"));
        assert!(locales.contains(&"de"));
        // Should be sorted
        assert_eq!(locales, {
            let mut sorted = locales.clone();
            sorted.sort_unstable();
            sorted
        });
    }

    #[test]
    fn overlay_translations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let trans_dir = tmp.path().join("translations");
        fs::create_dir_all(&trans_dir).unwrap();
        fs::write(
            trans_dir.join("en.json"),
            r#"{"custom_key": "custom_value"}"#,
        )
        .unwrap();
        let t = Translations::load(tmp.path());
        assert_eq!(t.get("en", "custom_key"), "custom_value");
        // Built-in keys should still be present
        assert_eq!(t.get("en", "save"), "Save");
    }

    /// Regression: a malformed overlay file was skipped without a word, so a
    /// typo silently reverted the whole locale to the built-in strings. The
    /// reason is reported and the other files still load.
    #[test]
    fn a_malformed_overlay_is_reported_and_the_rest_still_load() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let trans_dir = tmp.path().join("translations");
        fs::create_dir_all(&trans_dir).unwrap();
        fs::write(trans_dir.join("fr.json"), r#"{"save": "Enregistrer",}"#).unwrap();
        fs::write(trans_dir.join("nested.json"), r#"{"save": {"x": "y"}}"#).unwrap();
        fs::write(trans_dir.join("it.json"), r#"{"save": "Salva"}"#).unwrap();

        let reason = read_overlay(&trans_dir.join("fr.json")).unwrap_err();
        assert!(reason.contains("not a flat string map"), "{reason}");
        assert!(read_overlay(&trans_dir.join("nested.json")).is_err());

        let t = Translations::load(tmp.path());
        assert_eq!(t.get("it", "save"), "Salva");
        assert!(!t.available_locales().contains(&"fr"));
    }

    #[test]
    fn only_json_files_define_locales() {
        assert_eq!(overlay_locale(Path::new("/x/fr.json")), Some("fr"));
        assert_eq!(overlay_locale(Path::new("/x/README.md")), None);
    }

    #[test]
    fn overlay_adds_new_locale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let trans_dir = tmp.path().join("translations");
        fs::create_dir_all(&trans_dir).unwrap();
        fs::write(trans_dir.join("fr.json"), r#"{"save": "Enregistrer"}"#).unwrap();
        let t = Translations::load(tmp.path());
        assert_eq!(t.get("fr", "save"), "Enregistrer");
        // Unknown key in fr should fallback to en
        assert_eq!(t.get("fr", "cancel"), "Cancel");
    }
}
