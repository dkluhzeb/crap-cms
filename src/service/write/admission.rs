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
//!
//! What admission refuses in the input — a locale-locked field, a create in a
//! non-default locale — is decided here but raised only once the access gate
//! has admitted the caller ([`Admission::admit`]): the refusal names fields and
//! says which are shared across locales, which a caller without write access
//! must not learn. A group set to something other than an object is found here
//! and refused later still, once the field-level write strip has run
//! ([`NonObjectGroups`]): a group its writer may not write is dropped silently
//! like any other such field.

use serde_json::{Map, Value};

use crate::{
    core::{
        CollectionDefinition, DocumentFields, FieldDefinition, GlobalDefinition,
        canonicalize_text_values, nest_group_fields,
    },
    db::{LocaleContext, query},
    service::{
        ServiceContext, ServiceError, SnapshotLocales, SnapshotReadKeep, StoredByLocale,
        WriteHooks, WriteInput,
        write::{adopt_pending_draft, adopt_pending_global_draft, reject_locale_locked_fields},
    },
};

use super::{NonObjectGroups, validate::canonicalize_write_input};

type Result<T> = std::result::Result<T, ServiceError>;

/// Loads the stored document as one locale holds it (`None` without
/// localization), for the per-locale read judgement of a snapshot write-back.
pub(crate) type StoredLoader<'a> = dyn Fn(Option<&LocaleContext>) -> Result<DocumentFields> + 'a;

/// The stored document a publish's snapshot write-back is judged against: the
/// row as the write's locale reads it (every `access.update` rule's
/// `ctx.document`), and a loader for the row as each locale stores it (the
/// document each locale's `access.read` rules judge).
#[derive(Clone, Copy)]
pub(crate) struct PublishStored<'a> {
    row: &'a DocumentFields,
    load: &'a StoredLoader<'a>,
}

impl<'a> PublishStored<'a> {
    pub(crate) fn new(row: &'a DocumentFields, load: &'a StoredLoader<'a>) -> Self {
        Self { row, load }
    }
}

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
    /// publisher's own field-level write access and keeping every value the
    /// publisher cannot read.
    ///
    /// The pending draft goes live as ONE unit, so the locales the request does
    /// not target take their values from the snapshot too. The same rules that
    /// stripped the request's merged data run over the snapshot, judged — like
    /// every `access.update` rule — against the stored row rather than the
    /// content they are judging. Without this strip the write-back would publish
    /// exactly the drafted change the request strip refused. The completeness
    /// gate reads the same snapshot as its locale overlay.
    ///
    /// A write never changes a value its writer cannot read, and the
    /// write-back writes every locale: each locale's values the publisher
    /// cannot read in that locale — judged against the row as that locale
    /// stores it — are taken out, so the row keeps them as stored.
    ///
    /// # Errors
    ///
    /// Returns an internal error when the context carries no definition, or
    /// the error a stored-row read returns.
    pub(crate) fn publishing_snapshot(
        self,
        ctx: &ServiceContext,
        write_hooks: &dyn WriteHooks,
        stored: PublishStored<'_>,
        locale_ctx: Option<&LocaleContext>,
    ) -> Result<Option<Value>> {
        let Some(snapshot) = self.0 else {
            return Ok(None);
        };

        let fields = ctx.fields()?;
        let mut snapshot = Value::Object(snapshot);

        write_hooks.strip_write_access_value(
            fields,
            &mut snapshot,
            stored.row,
            ctx.slug,
            ctx.user,
            SnapshotLocales::for_write(locale_ctx),
        );

        // Without localization the write-back is skipped: the request's own
        // data carries the whole draft, and its strip kept what it hides. A
        // system write reads everything, so it keeps nothing.
        let Some(locale_ctx) = locale_ctx.filter(|c| c.config.is_enabled()) else {
            return Ok(Some(snapshot));
        };

        if write_hooks.overrides_access() {
            return Ok(Some(snapshot));
        }

        let by_locale = StoredByLocale::load(fields, Some(locale_ctx), stored.load)?;
        write_hooks.keep_unreadable_value(
            fields,
            &mut snapshot,
            &SnapshotReadKeep::builder(&by_locale, ctx.slug)
                .user(ctx.user)
                .locale_ctx(Some(locale_ctx))
                .build(),
        )?;

        Ok(Some(snapshot))
    }
}

/// What admission made of a write's input: the pending draft a publish
/// adopted, the input's refusal, if any — held until the access gate has
/// admitted the caller — and the request's non-object groups, held until the
/// field-level write strip has run.
#[must_use = "the refusal must be raised after the access gate"]
pub(crate) struct Admission {
    pending: PendingDraft,
    refusal: Option<ServiceError>,
    groups: NonObjectGroups,
}

impl Admission {
    /// An input admission accepts, with the request's non-object `groups`.
    fn accepted(pending: PendingDraft, groups: NonObjectGroups) -> Self {
        Self {
            pending,
            refusal: None,
            groups,
        }
    }

    /// An input admission refuses with `refusal`.
    fn refused(refusal: ServiceError) -> Self {
        Self {
            pending: PendingDraft::default(),
            refusal: Some(refusal),
            groups: NonObjectGroups::default(),
        }
    }

    /// Raise the input's refusal, if any, else hand back the pending draft and
    /// the request's non-object groups — to refuse
    /// ([`NonObjectGroups::refuse_unstripped`]) once the field-level write
    /// strip has run. Call it after the access gate.
    ///
    /// # Errors
    ///
    /// The refusal admission decided on.
    pub(crate) fn admit(self) -> Result<(PendingDraft, NonObjectGroups)> {
        match self.refusal {
            Some(refusal) => Err(refusal),
            None => Ok((self.pending, self.groups)),
        }
    }
}

/// Admit a create's input: canonicalize it (nested groups, canonical email and
/// text, untrusted upload metadata stripped), find the groups set to anything
/// but an object of its sub-fields, and refuse a non-default locale.
///
/// A document is created in its default (canonical) locale. A new row has no
/// default-locale value to translate from, so creating under a non-default
/// locale would write shared columns from the wrong locale AND leave the
/// default-locale columns empty. It is refused (parity with the update path's
/// locale lock); create in the default locale, then translate via update.
///
/// The refusal ([`Admission::admit`]) is a hook error when the write targets a
/// non-default locale.
pub(crate) fn admit_create_input(
    def: &CollectionDefinition,
    input: &mut WriteInput<'_>,
) -> Admission {
    canonicalize_write_input(input, def);

    if !query::is_non_default_single_locale(input.locale_ctx) {
        let groups = NonObjectGroups::of(&input.data, &def.fields);

        return Admission::accepted(PendingDraft::default(), groups);
    }

    Admission::refused(ServiceError::HookError(
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
/// The refusal ([`Admission::admit`]) is the locale lock's, on the merged
/// data. The non-object groups are found on the request's own data, before any
/// draft is adopted.
///
/// # Errors
///
/// Returns a backend error if the pending draft cannot be read.
pub(crate) fn admit_update_input(
    ctx: &ServiceContext,
    def: &CollectionDefinition,
    id: &str,
    input: &mut WriteInput<'_>,
) -> Result<Admission> {
    // Canonicalize up front (idempotent); the whole pipeline sees one shape
    // and the DB edge flattens to columns.
    canonicalize_write_input(input, def);

    // Found on the request's own data, before any draft is adopted.
    let groups = NonObjectGroups::of(&input.data, &def.fields);

    let pending = PendingDraft::new(adopt_pending_draft(ctx, def, id, input)?);

    Ok(locale_locked((pending, groups), &def.fields, input))
}

/// The admission of merged update data: refused by the locale lock, accepted
/// otherwise.
fn locale_locked(
    (pending, groups): (PendingDraft, NonObjectGroups),
    fields: &[FieldDefinition],
    input: &WriteInput<'_>,
) -> Admission {
    match reject_locale_locked_fields(fields, &input.data, input.locale_ctx) {
        Ok(()) => Admission::accepted(pending, groups),
        Err(refusal) => Admission::refused(refusal),
    }
}

/// Admit a global update's input — the same steps against the global's single
/// row. A global is never an upload collection, so canonicalizing is nesting
/// groups and storing canonical email and text.
///
/// The refusal is as for [`admit_update_input`].
///
/// # Errors
///
/// Returns a backend error if the pending draft cannot be read.
pub(crate) fn admit_global_update_input(
    ctx: &ServiceContext,
    def: &GlobalDefinition,
    input: &mut WriteInput<'_>,
) -> Result<Admission> {
    input.data = nest_group_fields(&input.data, &def.fields);
    canonicalize_text_values(&mut input.data, &def.fields);

    // Found on the request's own data, before any draft is adopted.
    let groups = NonObjectGroups::of(&input.data, &def.fields);

    let pending = PendingDraft::new(adopt_pending_global_draft(ctx, def, input)?);

    Ok(locale_locked((pending, groups), &def.fields, input))
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

        assert!(admit_create_input(&def, &mut input).admit().is_ok());

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

        let err = admit_create_input(&def, &mut input)
            .admit()
            .err()
            .expect("a non-default locale is refused");

        assert!(
            matches!(&err, ServiceError::HookError(msg) if msg.contains("non-default locale")),
            "got {err:?}"
        );
    }

    /// An update under a non-default locale that carries a shared field is
    /// refused by the admission — raised once the access gate has run.
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
            .unwrap()
            .admit()
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
    /// surface. The admission finds it — for a create and an update alike, and
    /// so for the dry-run too — and it is refused while the write strip keeps
    /// it.
    #[test]
    fn a_null_group_is_refused_on_create_and_update() {
        let def = with_seo();
        let ctx = ServiceContext::collection("posts", &def).build();

        let mut create = WriteInput::builder(data(&[("seo", Value::Null)])).build();
        let (_, groups) = admit_create_input(&def, &mut create).admit().unwrap();
        let err = groups
            .refuse_unstripped(&create.data)
            .expect_err("a null group is refused");
        assert!(
            matches!(&err, ServiceError::Validation(ve) if ve.to_field_map().contains_key("seo")),
            "got {err:?}"
        );

        let mut update = WriteInput::builder(data(&[("seo", Value::Null)])).build();
        let (_, groups) = admit_update_input(&ctx, &def, "p1", &mut update)
            .unwrap()
            .admit()
            .unwrap();
        let err = groups
            .refuse_unstripped(&update.data)
            .expect_err("a null group is refused");
        assert!(
            matches!(&err, ServiceError::Validation(ve) if ve.to_field_map().contains_key("seo")),
            "got {err:?}"
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod gate_tests {
    use anyhow::Result as AnyResult;
    use serde_json::{Map, Value, json};

    use crate::{
        config::{CrapConfig, LocaleConfig},
        core::{
            CollectionDefinition, DocumentFields, FieldAccess, FieldDefinition, FieldType, HookRef,
            Hooks, Registry, ValidationError,
        },
        db::{AccessResult, DbConnection, DbPool, LocaleContext, LocaleMode, migrate, pool},
        hooks::{
            AccessCheckInput, HookContext, HookEvent, ValidationCtx,
            lifecycle::access::WriteStripInput,
        },
        service::{
            FieldReadStrip, ServiceContext, ServiceError, WriteInput, create_document_in_conn,
            hooks::WriteHooks, update_document_in_conn,
        },
    };

    /// Write hooks running nothing, answering every access check with `.0`
    /// and denying every field-level write rule named `deny`.
    struct Gate(bool);

    impl WriteHooks for Gate {
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
            Ok(if self.0 {
                AccessResult::Allowed
            } else {
                AccessResult::Denied
            })
        }

        fn strip_write_access_map(
            &self,
            fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            input: &WriteStripInput<'_>,
        ) {
            let is_denied = |field: &FieldDefinition| {
                let rule = match input.operation {
                    "create" => field.access.create.as_ref(),
                    _ => field.access.update.as_ref(),
                };

                rule.is_some_and(|rule| rule.reference() == "deny")
            };

            level.retain(|key, _| !fields.iter().any(|f| &f.name == key && is_denied(f)));
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

    impl FieldReadStrip for Gate {}

    fn locales() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    /// `staff` holding a shared `slug`, a localized `bio` and a `payroll`
    /// group, on an `en`/`de` database.
    fn migrated_staff() -> (tempfile::TempDir, DbPool, CollectionDefinition) {
        let mut def = CollectionDefinition::new("staff");
        def.fields = vec![
            FieldDefinition::builder("slug", FieldType::Text).build(),
            FieldDefinition::builder("bio", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("payroll", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("band", FieldType::Text).build(),
                ])
                .build(),
        ];

        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def.clone());
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &locales()).unwrap();

        (tmp, db_pool, def)
    }

    fn data(pairs: &[(&str, Value)]) -> DocumentFields {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    fn is_denied(err: &ServiceError) -> bool {
        matches!(err, ServiceError::AccessDenied(_))
    }

    /// Regression: the group refusal ran before the access gate, so a caller
    /// with no create access learned which fields are groups ("payroll cannot
    /// be null") where every other input got a denial. The gate answers first;
    /// an admitted caller still gets the refusal.
    #[test]
    fn a_create_refusal_is_raised_only_past_the_access_gate() {
        let (_tmp, db_pool, def) = migrated_staff();
        let conn = db_pool.get().unwrap();
        let payroll_null = || WriteInput::builder(data(&[("payroll", Value::Null)])).build();

        let denied = Gate(false);
        let ctx = ServiceContext::collection("staff", &def)
            .conn(&conn)
            .write_hooks(&denied)
            .build();
        let err = create_document_in_conn(&ctx, payroll_null()).unwrap_err();
        assert!(is_denied(&err), "got {err:?}");

        let allowed = Gate(true);
        let ctx = ServiceContext::collection("staff", &def)
            .conn(&conn)
            .write_hooks(&allowed)
            .build();
        let err = create_document_in_conn(&ctx, payroll_null()).unwrap_err();
        assert!(matches!(err, ServiceError::Validation(_)), "got {err:?}");
    }

    /// Regression: the locale lock and the group refusal of an update ran
    /// before the access gate, naming shared fields and groups to a caller
    /// with no update access. The gate answers first.
    #[test]
    fn an_update_refusal_is_raised_only_past_the_access_gate() {
        let (_tmp, db_pool, def) = migrated_staff();
        let conn = db_pool.get().unwrap();

        let allowed = Gate(true);
        let writer = ServiceContext::collection("staff", &def)
            .conn(&conn)
            .write_hooks(&allowed)
            .build();
        // A localized collection is read back in a locale.
        let en = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: locales(),
        };
        let (doc, _) = create_document_in_conn(
            &writer,
            WriteInput::builder(data(&[("slug", json!("ada"))]))
                .locale_ctx(Some(&en))
                .build(),
        )
        .unwrap();
        let id = doc.id.to_string();

        let de = LocaleContext {
            mode: LocaleMode::Single("de".to_string()),
            config: locales(),
        };
        let denied = Gate(false);
        let outsider = ServiceContext::collection("staff", &def)
            .conn(&conn)
            .write_hooks(&denied)
            .build();

        let locked = WriteInput::builder(data(&[("slug", json!("neu"))]))
            .locale_ctx(Some(&de))
            .build();
        let err = update_document_in_conn(&outsider, &id, locked).unwrap_err();
        assert!(is_denied(&err), "locale lock: got {err:?}");

        let null_group = WriteInput::builder(data(&[("payroll", Value::Null)])).build();
        let err = update_document_in_conn(&outsider, &id, null_group).unwrap_err();
        assert!(is_denied(&err), "null group: got {err:?}");

        let locked = WriteInput::builder(data(&[("slug", json!("neu"))]))
            .locale_ctx(Some(&de))
            .build();
        let err = update_document_in_conn(&writer, &id, locked).unwrap_err();
        assert!(matches!(err, ServiceError::Validation(_)), "got {err:?}");
    }

    /// Regression: a group set to `null` was refused before the field-level
    /// write strip, so a caller whose field rule denies the group got a
    /// validation error instead of the documented silent strip. The strip
    /// drops it, on create and update alike, and the write goes through.
    #[test]
    fn a_null_group_its_writer_may_not_write_is_stripped_not_refused() {
        let (_tmp, db_pool, mut def) = migrated_staff();
        let conn = db_pool.get().unwrap();

        let payroll = def.fields.iter_mut().find(|f| f.name == "payroll").unwrap();
        payroll.access = FieldAccess {
            create: Some(HookRef::new("deny")),
            update: Some(HookRef::new("deny")),
            ..Default::default()
        };

        let allowed = Gate(true);
        let ctx = ServiceContext::collection("staff", &def)
            .conn(&conn)
            .write_hooks(&allowed)
            .build();

        // A localized collection is read back in a locale.
        let en = LocaleContext {
            mode: LocaleMode::Single("en".to_string()),
            config: locales(),
        };
        let request = || {
            WriteInput::builder(data(&[("slug", json!("ada")), ("payroll", Value::Null)]))
                .locale_ctx(Some(&en))
                .build()
        };

        let (doc, _) = create_document_in_conn(&ctx, request()).unwrap();
        assert_eq!(doc.get_str("slug"), Some("ada"));

        let id = doc.id.to_string();
        update_document_in_conn(&ctx, &id, request()).unwrap();
    }
}
