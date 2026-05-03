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
    "loading",
    "saving",
    "focal_point_hint",
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
        let json_str = json_str.replace("</", r"<\/");

        Ok(ScopedJson::Derived(Value::String(json_str)))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};

    use crate::admin::templates::helpers::test_helpers::test_hbs_with_translations;

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
        use super::ADMIN_JS_KEYS;
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
