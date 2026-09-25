//! The admission prefix of a write: what happens to the caller's data before
//! the access gate judges it.
//!
//! A create canonicalizes its input and refuses a non-default locale. An update
//! canonicalizes its input, adopts the pending draft a publish makes live, and
//! applies the locale lock to the result. These steps decide what the rest of
//! the pipeline — access rules, field hooks, validators — sees, so the real
//! writes and the `validate` dry-run both run them from here. A dry-run that
//! spelled them out a second time answered for a different write than the one
//! it previews: it missed the drafted fields a publish carries, the locale lock
//! and the upload-metadata strip.
//!
//! Only the non-locking part lives here. The real update takes the row lock
//! before it (a dry-run writes nothing and takes none) and the access gate and
//! the field-level write strip after it.

use serde_json::{Map, Value};

use crate::{
    core::{
        CollectionDefinition, DocumentFields, GlobalDefinition, canonicalize_text_values,
        nest_group_fields,
    },
    db::{LocaleContext, query},
    service::{
        ServiceContext, ServiceError, SnapshotLocales, WriteHooks, WriteInput,
        write::{adopt_pending_draft, adopt_pending_global_draft, reject_locale_locked_fields},
    },
};

use super::{group_values::reject_non_object_groups, validate::canonicalize_write_input};

type Result<T> = std::result::Result<T, ServiceError>;

/// The pending draft an update adopted as its base — empty unless the write
/// publishes a draft that was pending.
#[derive(Default)]
pub(crate) struct PendingDraft(Option<Map<String, Value>>);

impl PendingDraft {
    /// Wrap the snapshot an adoption returned.
    fn new(snapshot: Option<Map<String, Value>>) -> Self {
        Self(snapshot)
    }

    /// The snapshot the publish writes back over the row, stripped by the
    /// publisher's own field-level write access.
    ///
    /// The pending draft goes live as ONE unit, so the locales the request does
    /// not target take their values from the snapshot too. The same rules that
    /// stripped the request's merged data run over the snapshot, judged — like
    /// every `access.update` rule — against the `stored` row rather than the
    /// content they are judging. Without this strip the write-back would publish
    /// exactly the drafted change the request strip refused. The completeness
    /// gate reads the same snapshot as its locale overlay.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the context carries no definition.
    pub(crate) fn publishing_snapshot(
        self,
        ctx: &ServiceContext,
        write_hooks: &dyn WriteHooks,
        stored: &DocumentFields,
        locale_ctx: Option<&LocaleContext>,
    ) -> Result<Option<Value>> {
        let Some(snapshot) = self.0 else {
            return Ok(None);
        };

        let mut snapshot = Value::Object(snapshot);

        write_hooks.strip_write_access_value(
            ctx.fields()?,
            &mut snapshot,
            stored,
            ctx.slug,
            ctx.user,
            SnapshotLocales::for_write(locale_ctx),
        );

        Ok(Some(snapshot))
    }
}

/// Admit a create's input: canonicalize it (nested groups, canonical email and
/// text, untrusted upload metadata stripped), refuse a group set to anything
/// but an object of its sub-fields, and refuse a non-default locale.
///
/// A document is created in its default (canonical) locale. A new row has no
/// default-locale value to translate from, so creating under a non-default
/// locale would write shared columns from the wrong locale AND leave the
/// default-locale columns empty. It is refused (parity with the update path's
/// locale lock); create in the default locale, then translate via update.
///
/// # Errors
///
/// Returns a validation error for a group set to `null` or a non-object, and a
/// hook error when the write targets a non-default locale.
pub(crate) fn admit_create_input(
    def: &CollectionDefinition,
    input: &mut WriteInput<'_>,
) -> Result<()> {
    canonicalize_write_input(input, def);
    reject_non_object_groups(&input.data, &def.fields)?;

    if !query::is_non_default_single_locale(input.locale_ctx) {
        return Ok(());
    }

    Err(ServiceError::HookError(
        "Cannot create a document in a non-default locale — create in the default locale \
         first, then add translations with an update."
            .into(),
    ))
}

/// Admit an update's input: canonicalize it, adopt the pending draft a publish
/// makes live, then apply the locale lock to the merged data.
///
/// Publishing takes the pending draft as the write's base and lets the
/// request's own fields win over it — the file that draft stored included,
/// whose server-derived columns come from the snapshot, read after the upload
/// strip so they are the server's own values and not something a caller sent.
/// Everything the draft contributes then passes the locale lock, the access
/// gates and validation exactly like a field the caller sent. A draft save
/// (`input.draft`) publishes nothing and adopts nothing.
///
/// # Errors
///
/// Returns a backend error if the pending draft cannot be read, or the
/// locale-lock validation error.
pub(crate) fn admit_update_input(
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    id: &str,
    input: &mut WriteInput<'_>,
) -> Result<PendingDraft> {
    // Canonicalize up front (idempotent); the whole pipeline sees one shape
    // and the DB edge flattens to columns.
    canonicalize_write_input(input, def);
    reject_non_object_groups(&input.data, &def.fields)?;

    let pending = adopt_pending_draft(ctx, def, id, input)?;

    reject_locale_locked_fields(&def.fields, &input.data, input.locale_ctx)?;

    Ok(PendingDraft::new(pending))
}

/// Admit a global update's input — the same steps against the global's single
/// row. A global is never an upload collection, so canonicalizing is nesting
/// groups and storing canonical email and text.
///
/// # Errors
///
/// Returns a backend error if the pending draft cannot be read, or the
/// locale-lock validation error.
pub(crate) fn admit_global_update_input(
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    input: &mut WriteInput<'_>,
) -> Result<PendingDraft> {
    input.data = nest_group_fields(&input.data, &def.fields);
    canonicalize_text_values(&mut input.data, &def.fields);
    reject_non_object_groups(&input.data, &def.fields)?;

    let pending = adopt_pending_global_draft(ctx, def, input)?;

    reject_locale_locked_fields(&def.fields, &input.data, input.locale_ctx)?;

    Ok(PendingDraft::new(pending))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{FieldDefinition, FieldType, upload::CollectionUpload},
        db::LocaleMode,
    };

    fn de() -> LocaleContext {
        LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        }
    }

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    /// A create strips the server-derived upload columns a caller sent, so
    /// every step after it judges the same data the write persists.
    #[test]
    fn a_create_strips_untrusted_upload_metadata() {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload::new());

        let mut input = WriteInput::builder(data(&[
            ("filename", json!("forged.jpg")),
            ("caption", json!("hi")),
        ]))
        .build();

        admit_create_input(&def, &mut input).unwrap();

        assert!(!input.data.contains_key("filename"));
        assert_eq!(input.data.get("caption"), Some(&json!("hi")));
    }

    /// A create under a non-default locale is refused by the admission itself,
    /// so the dry-run refuses it exactly like the write.
    #[test]
    fn a_create_refuses_a_non_default_locale() {
        let def = CollectionDefinition::new("posts");
        let locale = de();
        let mut input = WriteInput::builder(DocumentFields::new())
            .locale_ctx(Some(&locale))
            .build();

        let err = admit_create_input(&def, &mut input).unwrap_err();

        assert!(
            matches!(&err, ServiceError::HookError(msg) if msg.contains("non-default locale")),
            "got {err:?}"
        );
    }

    /// An update under a non-default locale that carries a shared field is
    /// rejected by the admission, before any access rule or validator runs.
    #[test]
    fn an_update_applies_the_locale_lock() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("slug", FieldType::Text).build()];
        let ctx = ServiceContext::collection("posts", &def).build();

        let locale = de();
        let mut input = WriteInput::builder(data(&[("slug", json!("neu"))]))
            .locale_ctx(Some(&locale))
            .build();

        let err = admit_update_input(&ctx, &def, "p1", &mut input)
            .err()
            .expect("the shared field is locked under `de`");

        assert!(
            matches!(&err, ServiceError::Validation(ve) if ve.to_field_map().contains_key("slug")),
            "got {err:?}"
        );
    }

    fn with_seo() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        def
    }

    /// Regression: `{ seo = null }` was dropped without a word on every
    /// surface. The admission refuses it — for a create and an update alike,
    /// and so for the dry-run too.
    #[test]
    fn a_null_group_is_refused_on_create_and_update() {
        let def = with_seo();
        let ctx = ServiceContext::collection("posts", &def).build();

        let mut create = WriteInput::builder(data(&[("seo", Value::Null)])).build();
        let err = admit_create_input(&def, &mut create).unwrap_err();
        assert!(
            matches!(&err, ServiceError::Validation(ve) if ve.to_field_map().contains_key("seo")),
            "got {err:?}"
        );

        let mut update = WriteInput::builder(data(&[("seo", Value::Null)])).build();
        let err = admit_update_input(&ctx, &def, "p1", &mut update)
            .err()
            .expect("a null group is refused");
        assert!(
            matches!(&err, ServiceError::Validation(ve) if ve.to_field_map().contains_key("seo")),
            "got {err:?}"
        );
    }
}
