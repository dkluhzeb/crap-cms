//! Admin UI translation loading: compiled-in English + German, config dir overlay.

use std::collections::HashMap;
use std::path::Path;

static DEFAULT_EN: &str = include_str!("../../translations/en.json");
static DEFAULT_DE: &str = include_str!("../../translations/de.json");

/// Holds resolved translation strings for all locales.
pub struct Translations {
    locales: HashMap<String, HashMap<String, String>>,
}

impl Translations {
    /// Load translations: compiled-in locales as base, overlaid with
    /// `<config_dir>/translations/*.json` files if they exist.
    pub fn load(config_dir: &Path) -> Self {
        let mut locales: HashMap<String, HashMap<String, String>> = HashMap::new();

        // Load compiled-in defaults
        if let Ok(en) = serde_json::from_str::<HashMap<String, String>>(DEFAULT_EN) {
            locales.insert("en".to_string(), en);
        }
        if let Ok(de) = serde_json::from_str::<HashMap<String, String>>(DEFAULT_DE) {
            locales.insert("de".to_string(), de);
        }

        // Overlay with config dir translations/*.json if they exist
        let translations_dir = config_dir.join("translations");
        if translations_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&translations_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|ext| ext == "json") {
                        if let Some(locale) = path.file_stem().and_then(|s| s.to_str()) {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                if let Ok(overrides) = serde_json::from_str::<HashMap<String, String>>(&content) {
                                    let map = locales.entry(locale.to_string()).or_default();
                                    map.extend(overrides);
                                }
                            }
                        }
                    }
                }
            }
        }

        Translations { locales }
    }

    /// Get a translated string by locale and key.
    /// Falls back to "en" locale, then to the key itself.
    pub fn get<'a>(&'a self, locale: &str, key: &'a str) -> &'a str {
        // Try requested locale
        if let Some(strings) = self.locales.get(locale) {
            if let Some(val) = strings.get(key) {
                return val.as_str();
            }
        }
        // Fallback to English
        if locale != "en" {
            if let Some(strings) = self.locales.get("en") {
                if let Some(val) = strings.get(key) {
                    return val.as_str();
                }
            }
        }
        // Return key itself
        key
    }

    /// Get a translated string and interpolate `{{var}}` placeholders with the given params.
    pub fn get_interpolated(&self, locale: &str, key: &str, params: &HashMap<String, String>) -> String {
        let template = self.get(locale, key);
        if params.is_empty() {
            return template.to_string();
        }
        let mut result = template.to_string();
        for (k, v) in params {
            result = result.replace(&format!("{{{{{}}}}}", k), v);
        }
        result
    }

    /// Return the list of available locale codes.
    pub fn available_locales(&self) -> Vec<&str> {
        let mut locales: Vec<&str> = self.locales.keys().map(|s| s.as_str()).collect();
        locales.sort();
        locales
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
        assert_eq!(t.get("en", "nonexistent_key_12345"), "nonexistent_key_12345");
    }

    #[test]
    fn get_interpolated_replaces_vars() {
        let mut locales = HashMap::new();
        let mut en = HashMap::new();
        en.insert("greeting".to_string(), "Hello {{name}}, welcome to {{place}}!".to_string());
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
        let t = Translations { locales: HashMap::new() };
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
            sorted.sort();
            sorted
        });
    }

    #[test]
    fn overlay_translations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let trans_dir = tmp.path().join("translations");
        std::fs::create_dir_all(&trans_dir).unwrap();
        std::fs::write(
            trans_dir.join("en.json"),
            r#"{"custom_key": "custom_value"}"#,
        ).unwrap();
        let t = Translations::load(tmp.path());
        assert_eq!(t.get("en", "custom_key"), "custom_value");
        // Built-in keys should still be present
        assert_eq!(t.get("en", "save"), "Save");
    }

    #[test]
    fn overlay_adds_new_locale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let trans_dir = tmp.path().join("translations");
        std::fs::create_dir_all(&trans_dir).unwrap();
        std::fs::write(
            trans_dir.join("fr.json"),
            r#"{"save": "Enregistrer"}"#,
        ).unwrap();
        let t = Translations::load(tmp.path());
        assert_eq!(t.get("fr", "save"), "Enregistrer");
        // Unknown key in fr should fallback to en
        assert_eq!(t.get("fr", "cancel"), "Cancel");
    }
}
