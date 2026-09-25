use std::sync::Arc;

use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::{Map, Value};

use crate::admin::Translations;

/// Translation keys consumed by `static/components/` JavaScript via the
/// `i18n.js` `t(key)` helper. The server emits the resolved translations
/// for the active locale into a `<script type="application/json"
/// id="crap-i18n">` data island. Adding a new key in JS requires adding
/// it here so it ships to the browser.
///
/// Kept as a flat const so the audit grep for "what does the admin JS
/// translate" has one source of truth. Keys absent from the list still
/// work — the JS `t()` falls back to the key itself — but show up
/// untranslated.
const ADMIN_JS_KEYS: &[&str] = &[
    "filters",
    "columns",
    "save",
    "apply",
    "clear_all",
    "add_condition",
    "yes",
    "no",
    "value_placeholder",
    "cancel",
    "confirm",
    "unsaved_changes",
    "leave",
    "stay",
    "stay_logged_in",
    "log_out",
    "minute",
    "minutes",
    "session_expiry_warning",
    "no_results",
    "clear_selection",
    "browse",
    "browse_media",
    "load_more",
    "preview",
    "reload",
    "close",
    "another_user",
    "op_created",
    "op_updated",
    "op_deleted",
    "op_restored",
    "op_unpublished",
    "stale_deleted",
    "stale_updated",
    "op_is",
    "op_is_not",
    "op_contains",
    "op_equals",
    "op_gt",
    "op_lt",
    "op_gte",
    "op_lte",
    "op_after",
    "op_before",
    "op_on_or_after",
    "op_on_or_before",
    "op_exists",
    "op_not_exists",
    "op_and",
    "op_or",
    "filter_status_or_mixed",
    "status",
    "created",
    "updated",
    "published",
    "draft",
    "validation.error_summary",
    "validation.server_error",
    "link_url",
    "link_title",
    "link_open_new_tab",
    "link_nofollow",
    "insert_link",
    "edit_link",
    "remove_link",
    "validating",
    "move_to_trash",
    "delete_permanently",
    "delete_confirm_title",
    "delete_confirm_soft",
    "delete_confirm_hard",
    "moved_to_trash",
    "deleted_permanently",
    "delete_error",
    "empty_trash",
    "empty_trash_confirm_title",
    "empty_trash_confirm",
    "trash_emptied",
    "search",
    "search_to_add",
    "are_you_sure",
    "ok",
    "documents",
    "error",
    "no_details",
    "back_refs_some_inaccessible",
    "loading",
    "saving",
    "focal_point_hint",
    "upload_too_large",
    "request_failed",
    "search_failed",
    "unavailable_item",
    "remove_item",
    "remove_condition",
    "filter_connector",
    "filter_field",
    "filter_operator",
    "filter_value",
    "password_show",
    "code_language",
    "code_language_label",
    "richtext.bold",
    "richtext.italic",
    "richtext.code",
    "richtext.link",
    "richtext.heading_1",
    "richtext.heading_2",
    "richtext.heading_3",
    "richtext.paragraph",
    "richtext.bullet_list",
    "richtext.ordered_list",
    "richtext.blockquote",
    "richtext.quote",
    "richtext.horizontal_rule",
    "richtext.insert",
    "richtext.load_error",
    "richtext.undo",
    "richtext.undo_title",
    "richtext.redo",
    "richtext.redo_title",
];

/// Handlebars helper that emits the admin-JS i18n bundle as a single
/// JSON object string. Usage in the data-island:
///
/// ```hbs
/// <script type="application/json" id="crap-i18n" nonce="{{crap.csp_nonce}}">
///   {{{admin_i18n}}}
/// </script>
/// ```
///
/// The returned string is JSON-safe, with `</` escaped to `<\/` to
/// prevent `</script>` breakouts (mirroring `JsonHelper`). Used with
/// the triple-stash so handlebars does not HTML-escape the output.
pub(super) struct AdminI18nHelper {
    pub(super) translations: Arc<Translations>,
}

impl HelperDef for AdminI18nHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        _h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let locale = ctx
            .data()
            .get("_locale")
            .and_then(|v| v.as_str())
            .unwrap_or("en");

        let mut map = Map::with_capacity(ADMIN_JS_KEYS.len());
        for key in ADMIN_JS_KEYS {
            let value = self.translations.get(locale, key).to_string();
            map.insert((*key).to_string(), Value::String(value));
        }

        let json_str = serde_json::to_string(&Value::Object(map)).unwrap_or_default();
        // Mirror `JsonHelper` exactly: `</` so the payload can't close a
        // <script> element, AND `'` so the same payload stays inert if an
        // overlay ever moves it into a single-quoted attribute (both are
        // valid JSON escapes that parsers decode back).
        let json_str = json_str.replace("</", r"<\/").replace('\'', r"\u0027");

        Ok(ScopedJson::Derived(Value::String(json_str)))
    }
}

#[cfg(test)]
mod tests {
    /// Mirrors `json_escapes_single_quotes_for_html_attributes` on
    /// `JsonHelper` — the two raw-JSON-into-markup producers must share
    /// one escaping policy: a translation value
    /// containing `'` or `</script>` stays inert in both a script
    /// element and a single-quoted attribute.
    #[test]
    fn admin_i18n_escapes_mirror_the_json_helper() {
        let payload = serde_json::json!({ "k": "it's </script> tricky" });
        let json_str = serde_json::to_string(&payload).unwrap();
        let escaped = json_str.replace("</", r"<\/").replace('\'', r"\u0027");

        assert!(
            !escaped.contains("</"),
            "script-close must be broken: {escaped}"
        );
        assert!(
            !escaped.contains('\''),
            "single quotes must be escaped: {escaped}"
        );
        let back: serde_json::Value = serde_json::from_str(&escaped).unwrap();
        assert_eq!(
            back["k"], "it's </script> tricky",
            "escapes must be valid JSON"
        );
    }

    use super::ADMIN_JS_KEYS;
    use std::{collections::HashMap, fs, path::Path};

    use serde_json::{Value, json};

    use crate::admin::templates::helpers::test_helpers::test_hbs_with_translations;

    /// The keys a JavaScript source asks `t()` for.
    ///
    /// `t` has to stand alone: plenty of calls end in `t(` without being a
    /// translation (`document.createElement('div')`, `params.get('_method')`).
    fn translation_keys_in(source: &str) -> Vec<String> {
        let bytes = source.as_bytes();
        let mut keys = Vec::new();
        let mut at = 0;

        while let Some(offset) = source[at..].find("t('") {
            let start = at + offset;
            at = start + 3;

            let preceded_by_name = start
                .checked_sub(1)
                .map(|before| bytes[before])
                .is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.')
                });

            if preceded_by_name {
                continue;
            }

            let Some(end) = source[at..].find('\'') else {
                continue;
            };

            keys.push(source[at..at + end].to_string());
            at += end + 1;
        }

        keys
    }

    /// Every component source, nested directories included.
    fn collect_js(dir: &Path, out: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(dir)
            .expect("readable component directory")
            .flatten()
        {
            let path = entry.path();

            if path.is_dir() {
                collect_js(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "js") {
                let source = fs::read_to_string(&path).expect("readable component source");

                out.push((path.display().to_string(), source));
            }
        }
    }

    fn component_sources() -> Vec<(String, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("static/components");
        let mut out = Vec::new();

        collect_js(&root, &mut out);
        assert!(!out.is_empty(), "no component sources found under {root:?}");

        out
    }

    /// The scanner reads a standalone call and ignores an identifier that
    /// merely ends in `t`.
    #[test]
    fn the_scanner_reads_only_standalone_calls() {
        let source =
            "const a = t('save');\nconst b = document.createElement('div');\nq.get('_method');";

        assert_eq!(translation_keys_in(source), vec!["save".to_string()]);
    }

    /// Pin: every key the admin JS asks `t()` for is shipped in the data
    /// island, and every key the island ships has an English translation. A
    /// key missing from the list still renders — as its own bare name — which
    /// is how two live-event labels reached the toast untranslated.
    #[test]
    fn every_js_translation_key_ships_and_resolves() {
        let english: HashMap<String, String> =
            serde_json::from_str(include_str!("../../../../translations/en.json"))
                .expect("en.json is a flat string map");

        for (file, source) in component_sources() {
            for key in translation_keys_in(&source) {
                assert!(
                    ADMIN_JS_KEYS.contains(&key.as_str()),
                    "{file} translates '{key}' but ADMIN_JS_KEYS does not ship it"
                );
            }
        }

        for key in ADMIN_JS_KEYS {
            assert!(
                english.contains_key(*key),
                "ADMIN_JS_KEYS ships '{key}' but translations/en.json has no entry"
            );
        }
    }

    #[test]
    fn renders_valid_json_object_for_default_locale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{{admin_i18n}}}")
            .unwrap();

        let rendered = hbs.render("t", &json!({"_locale": "en"})).unwrap();
        let parsed: Value = serde_json::from_str(&rendered).expect("must be valid JSON");
        let obj = parsed.as_object().expect("must be an object");

        // Spot-check: one core key resolves to its English translation,
        // not the bare key.
        assert_eq!(
            obj.get("save").and_then(|v| v.as_str()),
            Some("Save"),
            "save must resolve to 'Save' in en, got: {rendered}"
        );
    }

    #[test]
    fn switches_locale_via_context() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{{admin_i18n}}}")
            .unwrap();

        let de = hbs.render("t", &json!({"_locale": "de"})).unwrap();
        let parsed: Value = serde_json::from_str(&de).expect("must be valid JSON");

        assert_eq!(
            parsed.get("save").and_then(|v| v.as_str()),
            Some("Speichern"),
            "save must resolve to 'Speichern' in de, got: {de}"
        );
    }

    #[test]
    fn includes_all_curated_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{{admin_i18n}}}")
            .unwrap();

        let rendered = hbs.render("t", &json!({"_locale": "en"})).unwrap();
        let parsed: Value = serde_json::from_str(&rendered).expect("must be valid JSON");
        let obj = parsed.as_object().unwrap();

        for key in ADMIN_JS_KEYS {
            assert!(obj.contains_key(*key), "missing key in admin_i18n: {key}");
        }
    }

    #[test]
    fn escapes_script_close_tag_in_translation_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let translations_dir = tmp.path().join("translations");
        fs::create_dir_all(&translations_dir).unwrap();
        // Pollute the `save` key with a `</script>` payload to verify
        // the escape path. (This would be sanitised at translation
        // ingestion in real life — the helper is the last line of
        // defence.)
        fs::write(
            translations_dir.join("en.json"),
            r#"{"save": "Save </script><script>alert(1)"}"#,
        )
        .unwrap();

        let mut hbs = test_hbs_with_translations(tmp.path());
        hbs.register_template_string("t", "{{{admin_i18n}}}")
            .unwrap();

        let rendered = hbs.render("t", &json!({"_locale": "en"})).unwrap();
        assert!(
            !rendered.contains("</script>"),
            "must escape </script>, got: {rendered}"
        );
        assert!(
            rendered.contains(r"<\/script>"),
            "must contain escaped form, got: {rendered}"
        );
    }
}
