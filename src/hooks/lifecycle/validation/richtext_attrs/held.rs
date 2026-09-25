//! Where a custom node's attr value is held: the same attr, on a node of the
//! same type, in the rich text field the edited document already stores.

use serde_json::Value;

use crate::{core::FieldDefinition, hooks::lifecycle::validation::stored::StoredDocument};

use super::extract::{KnownNodes, extract_nodes};

/// A node attr's position in the edited document.
pub(in crate::hooks::lifecycle::validation) struct NodeAttrSite<'a> {
    pub(super) stored: &'a StoredDocument<'a>,
    /// The rich text field the node sits in.
    pub(super) richtext: &'a FieldDefinition,
    pub(super) known_nodes: &'a KnownNodes<'a>,
    pub(super) node_type: &'a str,
}

impl NodeAttrSite<'_> {
    /// Whether the document already holds a value of attr `attr` on a node of
    /// this type in this rich text field that `matches`. Reads the document on
    /// first use.
    pub(in crate::hooks::lifecycle::validation) fn holds(
        &self,
        attr: &str,
        matches: impl Fn(&Value) -> bool,
    ) -> bool {
        self.stored.holds(self.richtext, |content| {
            extract_nodes(self.richtext, content, self.known_nodes)
                .iter()
                .filter(|inst| inst.node_type == self.node_type)
                .filter_map(|inst| inst.attrs.get(attr))
                .any(&matches)
        })
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{FieldAdmin, FieldType},
        db::InMemoryConn,
        hooks::lifecycle::validation::ValidationCtx,
    };

    fn body() -> FieldDefinition {
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(
                FieldAdmin::builder()
                    .richtext_format("json")
                    .nodes(vec!["alert".to_string(), "cta".to_string()])
                    .build(),
            )
            .build()
    }

    fn style() -> Vec<FieldDefinition> {
        vec![FieldDefinition::builder("style", FieldType::Select).build()]
    }

    /// A stored `body` with an `alert` whose style is `legacy`.
    fn conn() -> InMemoryConn {
        let doc = json!({ "type": "doc", "content": [
            { "type": "alert", "attrs": { "style": "legacy" } }
        ]})
        .to_string();

        let conn = InMemoryConn::open();
        conn.setup(&format!(
            "CREATE TABLE pages (id TEXT PRIMARY KEY, body TEXT);
             INSERT INTO pages VALUES ('p1', '{doc}');"
        ));
        conn
    }

    /// Held only on the same attr of the same node type.
    #[test]
    fn holds_the_attr_of_the_same_node_type_only() {
        let conn = conn();
        let fields = vec![body()];
        let attrs = style();
        let known: KnownNodes<'_> = [("alert", attrs.as_slice()), ("cta", attrs.as_slice())]
            .into_iter()
            .collect();
        let ctx = ValidationCtx::builder(&conn, "pages")
            .exclude_id(Some("p1"))
            .build();
        let stored = StoredDocument::new(&ctx, &fields);

        let site = |node_type: &'static str| NodeAttrSite {
            stored: &stored,
            richtext: &fields[0],
            known_nodes: &known,
            node_type,
        };
        let legacy = |v: &Value| v == &json!("legacy");

        assert!(site("alert").holds("style", legacy));
        assert!(!site("alert").holds("tone", legacy));
        assert!(!site("cta").holds("style", legacy));
    }
}
