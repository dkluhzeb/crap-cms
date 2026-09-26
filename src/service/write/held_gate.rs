//! The writer's [`HeldValueGate`]: which of the edited document's stored
//! values a write may resubmit unchanged although a check would now refuse
//! them (a retired select option, a rich text node the field no longer
//! enables).
//!
//! Accepting such a value says the document holds it, so it counts only where
//! the writer could read it: a field the writer may not read (`hidden`, or
//! denied by its `access.read` rule for this document) holds nothing for it,
//! and the pending draft holds nothing for a writer without the draft view.
//! Otherwise a write would confirm a guessed value of a field its writer
//! cannot see. A write with access overridden (a system write) sees
//! everything, hidden fields included, so it leans on every stored value.

use std::mem;

use tracing::warn;

use crate::{
    core::{Builder, Document, DocumentFields, FieldDefinition, HookRef},
    db::{AccessResult, query::filter::memory::matches_document},
    hooks::lifecycle::{AccessCheckInput, HeldSource, HeldValueGate},
    service::{ReadStripArgs, helpers::strip_unreadable, hooks::WriteHooks},
};

/// A write's [`HeldValueGate`], judged through its own write hooks (so an
/// access-overriding write leans on everything, hidden fields included). Built with the collection's
/// or global's draft view rule (`access.draft ?? access.update`).
#[derive(Builder)]
pub(crate) struct WriterHeldGate<'a> {
    #[builder(required)]
    write_hooks: &'a dyn WriteHooks,
    #[builder(required)]
    slug: &'a str,
    #[builder(required)]
    fields: &'a [FieldDefinition],
    draft_access: Option<&'a HookRef>,
    user: Option<&'a Document>,
    locale: Option<&'a str>,
}

impl WriterHeldGate<'_> {
    /// Whether the writer's draft view admits `draft`: allowed, or a row
    /// constraint the draft matches. A failing rule admits nothing.
    fn draft_view_admits(&self, draft: &Document) -> bool {
        let input = AccessCheckInput::builder("find_by_id", self.slug)
            .access(self.draft_access)
            .user(self.user)
            .locale(self.locale)
            .build();

        match self.write_hooks.check_access(&input) {
            Ok(AccessResult::Allowed) => true,
            Ok(AccessResult::Denied) => false,
            Ok(AccessResult::Constrained(filters)) => {
                matches_document(draft, &filters, self.fields)
            }
            Err(e) => {
                warn!(
                    slug = self.slug,
                    "draft view check failed, treating as denied: {e:#}"
                );
                false
            }
        }
    }
}

impl HeldValueGate for WriterHeldGate<'_> {
    fn admit(&self, source: HeldSource, fields: &mut DocumentFields) -> bool {
        if self.write_hooks.overrides_access() {
            return true;
        }

        let mut doc = Document::builder("").fields(mem::take(fields)).build();

        if source == HeldSource::PendingDraft && !self.draft_view_admits(&doc) {
            return false;
        }

        let args = ReadStripArgs::builder(self.fields, self.slug)
            .user(self.user)
            .locale(self.locale)
            .build();

        strip_unreadable(self.write_hooks, &args, &mut doc);

        *fields = doc.fields;

        true
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result as AnyResult;
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        core::{FieldAccess, FieldType, Hooks, ValidationError},
        db::DbConnection,
        hooks::{HookContext, HookEvent, ValidationCtx},
        service::FieldReadStrip,
    };

    /// Write hooks denying the draft view when `deny_draft`, and stripping
    /// `tier` on read — unless they `override_access`, which allows every
    /// check and strips nothing.
    #[derive(Default)]
    struct Writer {
        deny_draft: bool,
        override_access: bool,
    }

    impl WriteHooks for Writer {
        fn run_before_write(
            &self,
            _: &Hooks,
            _: &[FieldDefinition],
            ctx: HookContext,
            _: &ValidationCtx,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _: &Hooks,
            _: &[FieldDefinition],
            _: HookEvent,
            ctx: HookContext,
            _: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _: &Hooks,
            _: HookEvent,
            ctx: HookContext,
            _: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
            Ok(if self.deny_draft && !self.override_access {
                AccessResult::Denied
            } else {
                AccessResult::Allowed
            })
        }

        fn overrides_access(&self) -> bool {
            self.override_access
        }

        fn validate_fields(
            &self,
            _: &[FieldDefinition],
            _: &DocumentFields,
            _: &ValidationCtx,
        ) -> Result<(), ValidationError> {
            Ok(())
        }
    }

    impl FieldReadStrip for Writer {
        fn strip_read_access_map(
            &self,
            _: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _: &DocumentFields,
            _: &str,
            _: Option<&Document>,
            _: Option<&str>,
        ) {
            if !self.override_access {
                level.remove("tier");
            }
        }
    }

    fn fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition::builder("tier", FieldType::Select)
                .access(FieldAccess {
                    read: Some(HookRef::new("admins")),
                    ..Default::default()
                })
                .build(),
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("code", FieldType::Select)
                .hidden(true)
                .build(),
        ]
    }

    fn stored() -> DocumentFields {
        [
            ("tier", json!("gold")),
            ("title", json!("Hello")),
            ("code", json!("legacy")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }

    /// A field the writer may not read holds nothing for it; the rest of the
    /// source still counts.
    #[test]
    fn an_unreadable_field_holds_nothing() {
        let fields = fields();
        let writer = Writer::default();
        let gate = WriterHeldGate::builder(&writer, "staff", &fields).build();

        let mut source = stored();
        assert!(gate.admit(HeldSource::StoredRow, &mut source));
        assert_eq!(source.get("tier"), None);
        assert_eq!(source.get("code"), None, "a hidden field holds nothing");
        assert_eq!(source.get("title"), Some(&json!("Hello")));
    }

    /// The pending draft counts only with the writer's draft view.
    #[test]
    fn the_pending_draft_needs_the_draft_view() {
        let fields = fields();

        let without = Writer {
            deny_draft: true,
            ..Default::default()
        };
        let gate = WriterHeldGate::builder(&without, "staff", &fields).build();
        assert!(!gate.admit(HeldSource::PendingDraft, &mut stored()));
        assert!(
            gate.admit(HeldSource::StoredRow, &mut stored()),
            "the stored row is judged field by field"
        );

        let with = Writer::default();
        let gate = WriterHeldGate::builder(&with, "staff", &fields).build();
        assert!(gate.admit(HeldSource::PendingDraft, &mut stored()));
    }

    /// Regression: a hidden field held nothing even for a write with access
    /// overridden, so a system hook resubmitting a retired option it keeps in
    /// a hidden field was refused. A system write leans on every stored value
    /// of every source.
    #[test]
    fn an_access_overriding_write_leans_on_everything() {
        let fields = fields();
        let system = Writer {
            deny_draft: true,
            override_access: true,
        };
        let gate = WriterHeldGate::builder(&system, "staff", &fields).build();

        let mut source = stored();
        assert!(gate.admit(HeldSource::StoredRow, &mut source));
        assert_eq!(source, stored());

        assert!(gate.admit(HeldSource::PendingDraft, &mut stored()));
    }
}
