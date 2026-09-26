//! What the edited document already holds — for a value a write may resubmit
//! unchanged although a check that tightened since it was written would now
//! refuse it.

use std::{cell::OnceCell, ptr};

use anyhow::Result;
use serde_json::Value;
use tracing::warn;

use crate::{
    core::{DocumentFields, FieldDefinition, walk_nested},
    db::query::{StoredRow, find_pending_draft_fields, find_stored_fields},
};

use super::{HeldSource, ValidationCtx};

/// The edited document's stored values, read once and only when a check asks.
///
/// An update's stored row, plus its pending draft when drafts are kept as
/// versions — the edit form shows the draft, so a value it holds is one the
/// form resubmits. Nothing on create. Every source is read for the write's
/// locale, with its array and blocks rows.
pub(in crate::hooks::lifecycle::validation) struct StoredDocument<'a> {
    ctx: &'a ValidationCtx<'a>,
    fields: &'a [FieldDefinition],
    sources: OnceCell<Vec<DocumentFields>>,
}

impl<'a> StoredDocument<'a> {
    /// `fields` must be the very definitions the write is validated against:
    /// a stored value is matched to its field by identity.
    pub(in crate::hooks::lifecycle::validation) fn new(
        ctx: &'a ValidationCtx<'a>,
        fields: &'a [FieldDefinition],
    ) -> Self {
        Self {
            ctx,
            fields,
            sources: OnceCell::new(),
        }
    }

    /// The validation context this document belongs to.
    pub(in crate::hooks::lifecycle::validation) fn ctx(&self) -> &'a ValidationCtx<'a> {
        self.ctx
    }

    /// Whether the document already holds a value of `field` that `matches`.
    ///
    /// `field` is matched by identity, so a value counts only at its own schema
    /// position — the same sub-field in any row of the same array, which keeps
    /// a reordered row's value held. Reads the document on first use.
    pub(in crate::hooks::lifecycle::validation) fn holds(
        &self,
        field: &FieldDefinition,
        matches: impl Fn(&Value) -> bool,
    ) -> bool {
        let mut found = false;

        for source in self.sources() {
            walk_nested(
                source,
                self.fields,
                &mut Vec::new(),
                &mut |visited, value, _| {
                    if ptr::eq(visited, field) && value.is_some_and(&matches) {
                        found = true;
                    }
                },
            );
        }

        found
    }

    fn sources(&self) -> &[DocumentFields] {
        self.sources.get_or_init(|| self.read())
    }

    /// Read every source. A failed read holds nothing, so the value is judged
    /// on the check alone — the fail-closed direction.
    fn read(&self) -> Vec<DocumentFields> {
        let Some(id) = self.ctx.exclude_id else {
            return Vec::new();
        };

        let row = StoredRow {
            table: self.ctx.table,
            id,
            fields: self.fields,
            locale_ctx: self.ctx.locale_ctx,
        };

        let mut sources = Vec::new();
        sources.extend(self.admitted(
            HeldSource::StoredRow,
            logged(self.ctx.table, find_stored_fields(self.ctx.conn, &row)),
        ));

        if self.ctx.versioned_drafts {
            sources.extend(self.admitted(
                HeldSource::PendingDraft,
                logged(
                    self.ctx.table,
                    find_pending_draft_fields(self.ctx.conn, &row),
                ),
            ));
        }

        sources
    }

    /// `fields` as the writer may lean on them ([`HeldValueGate`]): stripped of
    /// what the writer may not read, or dropped when it may not see `source`.
    fn admitted(
        &self,
        source: HeldSource,
        fields: Option<DocumentFields>,
    ) -> Option<DocumentFields> {
        let mut fields = fields?;

        let Some(gate) = self.ctx.held_gate else {
            return Some(fields);
        };

        gate.admit(source, &mut fields).then_some(fields)
    }
}

fn logged(table: &str, read: Result<Option<DocumentFields>>) -> Option<DocumentFields> {
    read.inspect_err(|e| warn!(table, "could not read the stored document: {e:#}"))
        .ok()
        .flatten()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        core::{BlockDefinition, FieldType},
        db::InMemoryConn,
    };

    fn body() -> FieldDefinition {
        FieldDefinition::builder("body", FieldType::Textarea).build()
    }

    /// `body` at the top, and again inside a group inside a block row.
    fn fields() -> Vec<FieldDefinition> {
        vec![
            body(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "section",
                    vec![
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![body()])
                            .build(),
                    ],
                )])
                .build(),
        ]
    }

    fn conn() -> InMemoryConn {
        let conn = InMemoryConn::open();
        conn.setup(
            "CREATE TABLE pages (id TEXT PRIMARY KEY, body TEXT);
             CREATE TABLE pages_content (id TEXT PRIMARY KEY, parent_id TEXT, \
             _order INTEGER, _block_type TEXT, data TEXT);
             INSERT INTO pages VALUES ('p1', 'top');
             INSERT INTO pages_content VALUES ('b1', 'p1', 0, 'section', \
             '{\"meta\":{\"body\":\"nested\"}}');",
        );
        conn
    }

    fn nested_body(fields: &[FieldDefinition]) -> &FieldDefinition {
        &fields[1].blocks[0].fields[0].fields[0]
    }

    #[test]
    fn holds_values_at_their_own_schema_position_only() {
        let conn = conn();
        let fields = fields();
        let ctx = ValidationCtx::builder(&conn, "pages")
            .exclude_id(Some("p1"))
            .build();
        let stored = StoredDocument::new(&ctx, &fields);

        assert!(stored.holds(&fields[0], |v| v == &json!("top")));
        assert!(stored.holds(nested_body(&fields), |v| v == &json!("nested")));

        // Same field name, other position: not held there.
        assert!(!stored.holds(&fields[0], |v| v == &json!("nested")));
        assert!(!stored.holds(nested_body(&fields), |v| v == &json!("top")));
    }

    #[test]
    fn a_create_holds_nothing() {
        let conn = conn();
        let fields = fields();
        let ctx = ValidationCtx::builder(&conn, "pages").build();
        let stored = StoredDocument::new(&ctx, &fields);

        assert!(!stored.holds(&fields[0], |_| true));
    }

    /// The pending draft is a source too when drafts are versioned: the edit
    /// form shows it, so a value only the draft carries is resubmitted.
    #[test]
    fn a_pending_draft_is_held_when_drafts_are_versioned() {
        let conn = conn();
        conn.setup(
            "CREATE TABLE _versions_pages (id TEXT PRIMARY KEY, _parent TEXT, \
             _version INTEGER, _status TEXT, _latest INTEGER, snapshot TEXT, created_at TEXT);
             INSERT INTO _versions_pages VALUES ('v1', 'p1', 1, 'draft', 1, \
             '{\"body\":\"drafted\"}', NULL);",
        );
        let fields = fields();

        let plain = ValidationCtx::builder(&conn, "pages")
            .exclude_id(Some("p1"))
            .build();
        assert!(
            !StoredDocument::new(&plain, &fields).holds(&fields[0], |v| v == &json!("drafted"))
        );

        let versioned = ValidationCtx::builder(&conn, "pages")
            .exclude_id(Some("p1"))
            .versioned_drafts(true)
            .build();
        let stored = StoredDocument::new(&versioned, &fields);
        assert!(stored.holds(&fields[0], |v| v == &json!("drafted")));
        assert!(stored.holds(&fields[0], |v| v == &json!("top")));
    }
}
