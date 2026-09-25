//! Rich text value shape: an HTML-format value is text, a JSON-format value is
//! a document the field's editor can load.

use std::borrow::Cow;

use serde_json::{Value, from_str};

use crate::{
    core::{
        FieldDefinition, FieldType, Registry,
        richtext::{DocumentError, RichtextSchema, parse_document},
        validate::FieldError,
    },
    hooks::lifecycle::validation::{is_empty_value, stored::StoredDocument},
};

/// A rich text value under validation.
///
/// [`registry`](Self::registry) resolves the field's custom nodes (their attr
/// names); without one, custom node names are accepted unchecked.
/// [`stored`](Self::stored) attaches the edited document, whose own values stay
/// acceptable unchanged; without it the value is judged on the check alone.
pub(in crate::hooks::lifecycle::validation) struct RichtextCheck<'a> {
    field: &'a FieldDefinition,
    data_key: &'a str,
    value: Option<&'a Value>,
    registry: Option<&'a Registry>,
    stored: Option<&'a StoredDocument<'a>>,
}

impl<'a> RichtextCheck<'a> {
    pub(in crate::hooks::lifecycle::validation) fn new(
        field: &'a FieldDefinition,
        data_key: &'a str,
        value: Option<&'a Value>,
    ) -> Self {
        Self {
            field,
            data_key,
            value,
            registry: None,
            stored: None,
        }
    }

    /// The registry the field's custom nodes resolve in.
    #[must_use]
    pub(in crate::hooks::lifecycle::validation) fn registry(
        mut self,
        registry: Option<&'a Registry>,
    ) -> Self {
        self.registry = registry;

        self
    }

    /// The document the write lands on.
    #[must_use]
    pub(in crate::hooks::lifecycle::validation) fn stored(
        mut self,
        stored: &'a StoredDocument<'a>,
    ) -> Self {
        self.stored = Some(stored);

        self
    }
}

/// Refuse a rich text value its editor could not open — on every write
/// surface, whatever nodes the field declares. A JSON-format value must be a
/// `ProseMirror` document (its text, or the object itself) using only the node
/// and mark types the field enables; an HTML-format value must be text.
///
/// A value the edited document already holds in this field is accepted
/// unchanged, whatever it contains: disabling a feature must not make every
/// later save of a document written with it fail, nor lose the value — the
/// editor shows such a value read-only and resubmits it as stored. A value the
/// document does not already hold is still refused, so content the field no
/// longer allows can never be newly written. The document is read only when a
/// value fails, so an ordinary save costs no query.
pub(in crate::hooks::lifecycle::validation) fn check_richtext_value(
    check: &RichtextCheck<'_>,
    errors: &mut Vec<FieldError>,
) {
    let field = check.field;

    if field.field_type != FieldType::Richtext || is_empty_value(check.value) {
        return;
    }

    let Some(value) = check.value else {
        return;
    };

    let Some(error) = refusal(check, value) else {
        return;
    };

    let held = check
        .stored
        .is_some_and(|stored| stored.holds(field, |held| same_content(held, value)));

    if !held {
        errors.push(error);
    }
}

/// Why the editor could not open `value`, or `None` when it could.
fn refusal(check: &RichtextCheck<'_>, value: &Value) -> Option<FieldError> {
    let (field, data_key) = (check.field, check.data_key);

    if !field.parses_json() {
        return (!value.is_string()).then(|| {
            error(
                field,
                data_key,
                "must be HTML text",
                "validation.invalid_richtext_html",
            )
        });
    }

    parse_document(value)
        .ok_or(DocumentError::NotADocument)
        .and_then(|doc| RichtextSchema::for_field(field, check.registry).check(&doc))
        .err()
        .map(|e| document_error(field, data_key, &e))
}

/// Whether two rich text values carry the same content: JSON text compares as
/// the value it encodes, so a document read back as an object matches the text
/// the form resubmits, whatever its whitespace or key order.
fn same_content(a: &Value, b: &Value) -> bool {
    as_json(a) == as_json(b)
}

fn as_json(value: &Value) -> Cow<'_, Value> {
    match value {
        Value::String(s) => from_str(s).map_or(Cow::Borrowed(value), Cow::Owned),
        _ => Cow::Borrowed(value),
    }
}

fn error(field: &FieldDefinition, data_key: &str, message: &str, key: &str) -> FieldError {
    FieldError::with_key(data_key, format!("{} {message}", field.name), key)
        .with_param("field", field.name.clone())
}

fn document_error(field: &FieldDefinition, data_key: &str, e: &DocumentError) -> FieldError {
    match e {
        DocumentError::NodeNotAllowed(node) => error(
            field,
            data_key,
            &format!("contains a '{node}' element, which this field does not enable"),
            "validation.richtext_node_not_allowed",
        )
        .with_param("node", node.clone()),
        DocumentError::MarkNotAllowed(mark) => error(
            field,
            data_key,
            &format!("contains '{mark}' formatting, which this field does not enable"),
            "validation.richtext_mark_not_allowed",
        )
        .with_param("mark", mark.clone()),
        DocumentError::NotADocument | DocumentError::Malformed(_) => error(
            field,
            data_key,
            &format!("must be a valid rich text JSON document ({e})"),
            "validation.invalid_richtext_json",
        ),
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{DocumentFields, FieldAdmin, RichtextNodeDef},
        db::InMemoryConn,
        hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
    };

    fn richtext(format: &str, features: &[&str], nodes: &[&str]) -> FieldDefinition {
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .richtext_format(format)
                    .features(features.iter().map(ToString::to_string).collect())
                    .nodes(nodes.iter().map(ToString::to_string).collect())
                    .build(),
            )
            .build()
    }

    /// Validation error keys for `value` on `field`, through the full walker.
    fn keys(field: FieldDefinition, value: Value, registry: Option<&Registry>) -> Vec<String> {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");

        let data: DocumentFields = [("body".to_string(), value)].into_iter().collect();
        let mut builder = ValidationCtx::builder(&conn, "test");
        if let Some(registry) = registry {
            builder = builder.registry(registry);
        }

        validate_fields_inner(&lua, &[field], &data, &builder.build())
            .err()
            .map(|e| e.errors.iter().filter_map(|fe| fe.key.clone()).collect())
            .unwrap_or_default()
    }

    /// Validation error keys for an update of document `d1`, whose stored
    /// `body` is `stored`, submitting `value`.
    fn update_keys(field: FieldDefinition, stored: &Value, value: Value) -> Vec<String> {
        let lua = mlua::Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)");
        conn.setup(&format!(
            "INSERT INTO test VALUES ('d1', '{}')",
            stored.to_string().replace('\'', "''")
        ));

        let data: DocumentFields = [("body".to_string(), value)].into_iter().collect();
        let ctx = ValidationCtx::builder(&conn, "test")
            .exclude_id(Some("d1"))
            .build();

        validate_fields_inner(&lua, &[field], &data, &ctx)
            .err()
            .map(|e| e.errors.iter().filter_map(|fe| fe.key.clone()).collect())
            .unwrap_or_default()
    }

    fn doc_with(node: Value) -> Value {
        let mut doc = json!({ "type": "doc" });
        doc["content"] = Value::Array(vec![node]);

        doc
    }

    /// Regression: a JSON-format value was only checked when the field had
    /// custom nodes with attrs, so a string, number or list was stored and read
    /// back as something other than a document — which the editor showed as
    /// empty and overwrote on the next keystroke.
    #[test]
    fn json_format_refuses_non_documents_without_custom_nodes() {
        for value in [
            json!("hello"),
            json!(42),
            json!(true),
            json!([1, 2]),
            json!("[1,2]"),
        ] {
            assert_eq!(
                keys(richtext("json", &[], &[]), value.clone(), None),
                vec!["validation.invalid_richtext_json"],
                "{value}"
            );
        }
    }

    #[test]
    fn json_format_accepts_documents_in_both_forms() {
        let doc = doc_with(json!({ "type": "paragraph", "content": [
            { "type": "text", "text": "Hi", "marks": [{ "type": "strong" }] }
        ]}));

        assert!(keys(richtext("json", &[], &[]), doc.clone(), None).is_empty());
        assert!(keys(richtext("json", &[], &[]), json!(doc.to_string()), None).is_empty());
    }

    /// A node or mark the field does not enable is refused: the editor could
    /// not load the document.
    #[test]
    fn json_format_refuses_disabled_nodes_and_marks() {
        let heading = doc_with(json!({ "type": "heading", "attrs": { "level": 1 } }));
        assert_eq!(
            keys(richtext("json", &["bold"], &[]), heading, None),
            vec!["validation.richtext_node_not_allowed"]
        );

        let italic = doc_with(json!({ "type": "paragraph", "content": [
            { "type": "text", "text": "x", "marks": [{ "type": "em" }] }
        ]}));
        assert_eq!(
            keys(richtext("json", &["bold"], &[]), italic, None),
            vec!["validation.richtext_mark_not_allowed"]
        );
    }

    #[test]
    fn json_format_refuses_unregistered_custom_nodes() {
        let mut registry = Registry::new();
        registry.register_richtext_node(RichtextNodeDef::builder("cta", "CTA").build());

        let cta = doc_with(json!({ "type": "cta" }));
        assert!(
            keys(
                richtext("json", &[], &["cta"]),
                cta.clone(),
                Some(&registry)
            )
            .is_empty()
        );
        assert_eq!(
            keys(richtext("json", &[], &[]), cta, Some(&registry)),
            vec!["validation.richtext_node_not_allowed"]
        );
    }

    /// A field with custom nodes reports a non-document once — the node-attr
    /// pass leaves it to this check.
    #[test]
    fn non_document_with_custom_nodes_is_reported_once() {
        let mut registry = Registry::new();
        registry.register_richtext_node(
            RichtextNodeDef::builder("cta", "CTA")
                .attrs(vec![
                    FieldDefinition::builder("url", FieldType::Text)
                        .required(true)
                        .build(),
                ])
                .build(),
        );

        assert_eq!(
            keys(
                richtext("json", &[], &["cta"]),
                json!("not json"),
                Some(&registry)
            ),
            vec!["validation.invalid_richtext_json"]
        );
    }

    #[test]
    fn html_format_requires_text() {
        assert!(keys(richtext("html", &[], &[]), json!("<p>x</p>"), None).is_empty());
        assert_eq!(
            keys(richtext("html", &[], &[]), json!({ "type": "doc" }), None),
            vec!["validation.invalid_richtext_html"]
        );
    }

    fn heading(text: &str) -> Value {
        doc_with(
            json!({ "type": "heading", "attrs": { "level": 1 }, "content": [
                { "type": "text", "text": text }
            ]}),
        )
    }

    /// Regression: a value written before a feature was disabled could not be
    /// saved again — the editor cannot load it, so it resubmits it as stored,
    /// and the check refused it. The value the document already holds is
    /// accepted unchanged, in either form.
    #[test]
    fn a_held_value_is_accepted_unchanged() {
        let stored = heading("Kept");

        assert!(update_keys(richtext("json", &["bold"], &[]), &stored, stored.clone()).is_empty());
        assert!(
            update_keys(
                richtext("json", &["bold"], &[]),
                &stored,
                json!(stored.to_string())
            )
            .is_empty()
        );
    }

    /// A changed value is judged on the check alone: content the field no
    /// longer allows can never be newly written.
    #[test]
    fn a_changed_value_the_field_no_longer_allows_is_refused() {
        assert_eq!(
            update_keys(
                richtext("json", &["bold"], &[]),
                &heading("Kept"),
                heading("Changed")
            ),
            vec!["validation.richtext_node_not_allowed"]
        );
    }

    /// A create has no stored document to hold anything.
    #[test]
    fn a_create_holds_nothing() {
        assert_eq!(
            keys(richtext("json", &["bold"], &[]), heading("Kept"), None),
            vec!["validation.richtext_node_not_allowed"]
        );
    }
}
