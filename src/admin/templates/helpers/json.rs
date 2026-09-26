use handlebars::{Handlebars, Helper, HelperDef, RenderContext, RenderError, ScopedJson};
use serde_json::{Value, to_string};

/// JSON serialization helper: `{{{json value}}}`.
pub(super) struct JsonHelper;

/// `value` as JSON text that stays inert wherever a template places it raw: in
/// a `<script>` element or inside a single-quoted attribute. Every helper that
/// emits raw JSON into markup goes through here, so the escaping policy is
/// one policy.
pub(super) fn markup_json(value: &Value) -> String {
    let json_str = to_string(value).unwrap_or_default();

    // Prevent </script> breakout when used inside <script> blocks.
    let json_str = json_str.replace("</", r"<\/");

    // Prevent HTML attribute breakout when used in single-quoted attributes
    // like data-condition='{{{json condition_json}}}'.
    // \u0027 is valid JSON — parsers decode it back to '.
    let json_str = json_str.replace('\'', r"\u0027");

    // An HTML attribute value decodes character references before the value
    // reaches `JSON.parse`, so a string holding `&amp;` would arrive as `&`.
    // `&` only occurs inside JSON strings, where `\u0026` is the same
    // character.
    json_str.replace('&', r"\u0026")
}

impl HelperDef for JsonHelper {
    fn call_inner<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'rc>,
        _r: &'reg Handlebars<'reg>,
        _ctx: &'rc handlebars::Context,
        _rc: &mut RenderContext<'reg, 'rc>,
    ) -> Result<ScopedJson<'rc>, RenderError> {
        let val = h
            .param(0)
            .map_or(&Value::Null, handlebars::PathAndJson::value);

        Ok(ScopedJson::Derived(Value::String(markup_json(val))))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, from_str, json};

    use crate::admin::templates::helpers::test_helpers::test_hbs;

    #[test]
    fn json_object() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &json!({"val": {"key": "value"}})).unwrap();
        assert!(result.contains("\"key\""));
        assert!(result.contains("\"value\""));
    }

    #[test]
    fn json_null() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &json!({"val": null})).unwrap();
        assert_eq!(result, "null");
    }

    #[test]
    fn json_array() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &json!({"val": [1, "two", true]})).unwrap();
        assert_eq!(result, r#"[1,"two",true]"#);
    }

    #[test]
    fn json_string() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &json!({"val": "hello"})).unwrap();
        assert_eq!(result, "\"hello\"");
    }

    #[test]
    fn json_no_param() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json}}}").unwrap();
        let result = hbs.render("t", &json!({})).unwrap();
        assert_eq!(result, "null");
    }

    #[test]
    fn json_escapes_single_quotes_for_html_attributes() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &json!({"val": "it's a test"})).unwrap();
        assert!(
            !result.contains('\''),
            "must not contain literal single quote: {result}"
        );
        assert!(
            result.contains(r"\u0027"),
            r"should escape ' to \u0027: {result}"
        );
    }

    /// Regression: a condition value spelled like a character reference
    /// (`&amp;`) was decoded by the HTML parser inside `data-condition='…'`,
    /// so the browser compared against `&` while the server compared against
    /// `&amp;`.
    #[test]
    fn json_escapes_ampersands_for_html_attributes() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs.render("t", &json!({"val": "a &amp; b"})).unwrap();

        assert_eq!(result, r#""a \u0026amp; b""#);
        assert_eq!(from_str::<Value>(&result).unwrap(), json!("a &amp; b"));
    }

    #[test]
    fn json_escapes_script_close_tag() {
        let mut hbs = test_hbs();
        hbs.register_template_string("t", "{{{json val}}}").unwrap();
        let result = hbs
            .render("t", &json!({"val": "break</script><script>alert(1)"}))
            .unwrap();
        assert!(
            result.contains(r"<\/script>"),
            "should escape </script> to <\\/script>: {result}"
        );
        assert!(
            !result.contains("</script>"),
            "must not contain literal </script>"
        );
    }
}
