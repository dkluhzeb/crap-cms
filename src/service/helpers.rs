//! Shared helper functions for the service layer.

use serde_json::{Map, Value};

use crate::{
    config::{PasswordPolicy, PasswordViolation},
    core::{
        Document, FieldDefinition, FieldDenial, ReqContext,
        collection::Hooks,
        upload,
        validate::{FieldError, ValidationError},
    },
    db::{
        AccessResult, DbConnection, Filter, FilterClause, FilterOp, FindQuery, LocaleContext,
        query::{self, filter::normalize_order_by},
    },
    hooks::{HookContext, HookEvent, lifecycle::access::collect_denials_flat},
    service::{
        AfterChangeInput, FieldReadStrip, ReadStripArgs, ServiceContext, ServiceError,
        hooks::WriteHooks,
    },
};

/// Hydrate the join fields of the stored row a write reports — for the write's
/// locale, or the default locale without one — before `after_change` hooks, the
/// caller and the event see it.
///
/// Needed only for a document read without its rows: the flat re-read a write
/// query returns (`query::create`, `query::update`, `query::restore_version`)
/// or a `find_by_id_raw`. A document from `find_by_id` or `get_global` already
/// carries its rows, and a draft save reports its snapshot, which does too —
/// hydrating those again reads the join tables twice.
///
/// # Errors
///
/// Returns an error when no connection or definition is attached, or a join
/// table read fails.
pub(crate) fn hydrate_reported(
    ctx: &ServiceContext,
    doc: &mut Document,
    locale_ctx: Option<&LocaleContext>,
) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    let default = ctx.default_locale_ctx();
    let locale_ctx = locale_ctx.or(default.as_ref());

    query::hydrate_document(
        conn.as_ref(),
        &ctx.version_table(),
        ctx.fields()?,
        doc,
        None,
        locale_ctx,
    )?;

    Ok(())
}

/// Shape a reported document as a read returns it: an upload document's
/// per-size values folded into `sizes`. Strips nothing — a write strips for its
/// caller afterwards ([`strip_reported`]), and a live event's document, shaped
/// here by the publisher, is stripped per subscriber on delivery.
pub(crate) fn shape_reported(ctx: &ServiceContext, doc: &mut Document) {
    let Ok(def) = ctx.collection_def() else {
        return;
    };

    upload::shape_read_document(def, doc);
}

/// Shape the document a write reports as a read returns it ([`shape_reported`]),
/// then strip what the caller may not read: read-denied fields, judged in the
/// write's locale (the default locale without one), and hidden fields.
///
/// # Errors
///
/// Returns an error when the context has no definition.
pub(crate) fn strip_reported(
    ctx: &ServiceContext,
    write_hooks: &dyn WriteHooks,
    doc: &mut Document,
    locale_ctx: Option<&LocaleContext>,
) -> Result<(), ServiceError> {
    let fields = ctx.fields()?;
    let default = ctx.default_locale_ctx();
    let locale = locale_ctx
        .or(default.as_ref())
        .map(LocaleContext::access_locale);

    shape_reported(ctx, doc);

    strip_unreadable(
        write_hooks,
        &ReadStripArgs::builder(fields, ctx.slug)
            .user(ctx.user)
            .locale(locale)
            .build(),
        doc,
    );

    Ok(())
}

/// Strip what a reader may not read from a document: first the read-denied
/// fields — `strip`'s data-aware field-read strip, which judges the document
/// before anything else is removed — then the hidden fields.
pub(crate) fn strip_unreadable(
    strip: &dyn FieldReadStrip,
    args: &ReadStripArgs<'_>,
    doc: &mut Document,
) {
    strip.strip_read_access_doc(args.fields, doc, args.collection, args.user, args.locale);

    doc.strip_fields(&collect_api_hidden_field_names(args.fields, ""));
}

/// [`strip_unreadable`] for a batch of documents: the read-denied fields of the
/// whole batch are stripped at once, so the hooks evaluate it in one pass; the
/// hidden fields are collected once.
pub(crate) fn strip_unreadable_docs(
    strip: &dyn FieldReadStrip,
    args: &ReadStripArgs<'_>,
    docs: &mut [Document],
) {
    strip.strip_read_access_docs(args.fields, docs, args.collection, args.user, args.locale);

    let hidden = collect_api_hidden_field_names(args.fields, "");
    if hidden.is_empty() {
        return;
    }

    for doc in docs.iter_mut() {
        doc.strip_fields(&hidden);
    }
}

/// [`strip_unreadable`] for the map/fields shape a live event carries: the
/// caller's `strip_read_access` runs first — judging the payload before
/// anything is removed — then the hidden fields go.
pub(crate) fn strip_unreadable_fields(
    fields: &[FieldDefinition],
    level: &mut Map<String, Value>,
    strip_read_access: impl FnOnce(&mut Map<String, Value>),
) {
    strip_read_access(level);

    for denial in collect_api_hidden_field_names(fields, "") {
        denial.strip_from(level);
    }
}

/// Validate a supplied auth-collection `password` against the effective policy,
/// surfaced as a structured `password` field error.
///
/// This is THE authoritative password-policy enforcement point: the service
/// create/update chokepoint calls it for every surface and every op (single and
/// bulk), so no weak password can reach the DB regardless of which caller wrote
/// it. A `None` policy falls back to [`PasswordPolicy::default`] — the policy is
/// *always* enforced, so a context that forgets to thread the configured policy
/// degrades to the default rules, never to no enforcement. No-op for a non-auth
/// collection or an absent password; what an empty one means is the caller's
/// [`EmptyPassword`] choice.
///
/// # Errors
///
/// Returns [`ServiceError::Validation`] with a single `password` field error
/// when the password violates the policy, or is empty where empty is rejected.
pub(crate) fn validate_password_policy(
    is_auth: bool,
    password: Option<&str>,
    policy: Option<&PasswordPolicy>,
    empty: EmptyPassword,
) -> Result<(), ServiceError> {
    if !is_auth {
        return Ok(());
    }

    // Absent is always fine: an auth document may legitimately have no
    // password (an external auth method owns the credential).
    let Some(pw) = password else {
        return Ok(());
    };

    if pw.is_empty() {
        return match empty {
            EmptyPassword::MeansNoChange => Ok(()),
            EmptyPassword::IsRejected => Err(password_error(FieldError::with_key(
                "password",
                "Password must not be empty",
                "validation.password_empty",
            ))),
        };
    }

    let default_policy = PasswordPolicy::default();
    let policy = policy.unwrap_or(&default_policy);

    policy
        .validate(pw)
        .map_err(|violation| password_error(violation_error(violation)))
}

/// What a present-but-empty `password` means to the caller.
#[derive(Clone, Copy, Debug)]
pub(crate) enum EmptyPassword {
    /// Update: there is a stored password, and an empty value means "leave
    /// it alone".
    MeansNoChange,
    /// Create: there is nothing to leave alone, so an empty value is a
    /// caller error rather than a silently passwordless account.
    IsRejected,
}

/// The `password` field error for a policy violation: its English message,
/// plus the translation key and params the admin renders it with.
fn violation_error(violation: PasswordViolation) -> FieldError {
    violation.params().into_iter().fold(
        FieldError::with_key(
            "password",
            violation.to_string(),
            violation.translation_key(),
        ),
        |error, (name, value)| error.with_param(name, value),
    )
}

fn password_error(error: FieldError) -> ServiceError {
    ServiceError::Validation(ValidationError::new(vec![error]))
}

/// Run after-change hooks and return the request-scoped context.
/// This pattern is repeated across create, update, unpublish, and global update.
pub(crate) fn run_after_change_hooks(
    write_hooks: &dyn WriteHooks,
    hooks: &Hooks,
    fields: &[FieldDefinition],
    doc: &Document,
    input: AfterChangeInput<'_>,
    tx: &dyn DbConnection,
) -> anyhow::Result<ReqContext> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));
    let after_ctx = HookContext::builder(input.slug, input.operation)
        .data(after_data)
        .document_id(doc.id.to_string())
        .draft(input.is_draft)
        .locale(input.locale)
        .context(input.req_context)
        .user(input.user)
        .ui_locale(input.ui_locale)
        .build();
    let after_result =
        write_hooks.run_after_write(hooks, fields, HookEvent::AfterChange, after_ctx, tx)?;
    Ok(after_result.context)
}

/// A write that moves a document between states without touching its fields.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StateChange {
    /// `_status` moves to `draft`: the document is a draft afterwards.
    Unpublish,
    /// The row comes back out of the trash.
    Undelete,
}

impl StateChange {
    /// The `ctx.operation` hooks see. Unpublishing is an `update` to a hook —
    /// the same operation the admin form's status toggle performs — while an
    /// undelete names itself, matching the event it publishes.
    fn operation(self) -> &'static str {
        match self {
            Self::Unpublish => "update",
            Self::Undelete => "undelete",
        }
    }

    /// Whether the document is a draft once the write has landed.
    fn is_draft(self) -> bool {
        matches!(self, Self::Unpublish)
    }

    /// The after-change input this state write hands its hooks, carrying the
    /// request context its `before_change` produced — so the pair around one
    /// write can never name the operation or the draft flag differently.
    pub(crate) fn after_change<'a>(
        self,
        ctx: &'a ServiceContext,
        locale_ctx: Option<&LocaleContext>,
        req_context: ReqContext,
    ) -> AfterChangeInput<'a> {
        AfterChangeInput::builder(ctx.slug, self.operation())
            .draft(self.is_draft())
            .locale(locale_ctx.map(|lc| lc.access_locale().to_string()))
            .req_context(req_context)
            .user(ctx.user)
            .ui_locale(ctx.ui_locale.as_deref())
            .build()
    }
}

/// Run `before_change` for a state write and return its request context.
///
/// The stored document is the hook's `ctx.data`, and a hook may not replace it
/// — a state write carries no field edits — so only the request context
/// travels on to [`run_after_change_hooks`]. An error here aborts the write
/// before anything has moved. Unpublish and undelete share this so neither can
/// drift on what a hook sees.
///
/// # Errors
///
/// Returns the hook's own error, or an internal error when no connection,
/// write hooks or collection definition is attached.
pub(crate) fn run_state_before_change(
    ctx: &ServiceContext,
    change: StateChange,
    doc: &Document,
    locale_ctx: Option<&LocaleContext>,
) -> Result<ReqContext, ServiceError> {
    let conn = ctx.resolve_conn()?;
    let write_hooks = ctx.write_hooks()?;
    let def = ctx.collection_def()?;

    let hook_ctx = HookContext::builder(ctx.slug, change.operation())
        .data(doc.fields.clone())
        .document_id(doc.id.to_string())
        .draft(change.is_draft())
        .locale(locale_ctx.map(LocaleContext::access_locale))
        .user(ctx.user)
        .ui_locale(ctx.ui_locale.as_deref())
        .build();

    let final_ctx = write_hooks.run_hooks_with_conn(
        &def.hooks,
        HookEvent::BeforeChange,
        hook_ctx,
        conn.as_ref(),
    )?;

    Ok(final_ctx.context)
}

/// Collect denials for fields marked top-level `hidden = true`, for stripping
/// from API read responses (gRPC, Lua, MCP, admin JSON, REST). Covers flat
/// columns, group subfields (`__` prefix), and fields nested inside array/blocks
/// rows at any depth — sharing the [`collect_denials_flat`] walker with
/// field-access so the two never diverge.
///
/// `admin.hidden` is *not* read here — that flag controls admin-form rendering
/// only and never affects API output (so the admin upload widget, gRPC, Lua,
/// etc. can read auto-injected upload meta like `url`, `mime_type`, `focal_x`).
pub(crate) fn collect_api_hidden_field_names(
    fields: &[FieldDefinition],
    prefix: &str,
) -> Vec<FieldDenial> {
    let is_hidden = |field: &FieldDefinition| field.hidden;

    let mut hidden = Vec::new();
    collect_denials_flat(fields, &is_hidden, prefix, &mut hidden);

    hidden
}

/// Enforce a write-access `Constrained` result against a specific target row.
///
/// When a write access hook returns [`AccessResult::Constrained(filters)`],
/// operators expect the extra filters to scope the write to matching rows
/// (e.g. "users can only update their own rows"). The write paths have no
/// in-memory filter evaluator, so this helper piggybacks on the query layer:
/// it counts rows matching `filters AND id = <id>` and rejects the write
/// (returns [`ServiceError::AccessDenied`]) when zero rows match.
///
/// Non-`Constrained` variants are a no-op — callers handle `Allowed`/`Denied`
/// themselves before the write. `locale_ctx` is passed as `None` because
/// access-hook constraints are almost always locale-independent identity
/// filters (`author_id = X`), and the target row exists in some locale.
///
/// `include_deleted` must be true for undelete (the target row is in the
/// trash) and false everywhere else. `operation` is used only for the error
/// message ("Update access denied", "Delete access denied", …).
pub(crate) fn enforce_access_constraints(
    ctx: &ServiceContext,
    id: &str,
    access: &AccessResult,
    operation: &str,
    include_deleted: bool,
) -> Result<(), ServiceError> {
    let AccessResult::Constrained(extra) = access else {
        return Ok(());
    };

    let conn = ctx.resolve_conn()?;
    let conn = conn.as_ref();
    let def = ctx.collection_def()?;

    let mut filters: Vec<FilterClause> = extra.clone();
    filters.push(FilterClause::Single(Filter {
        field: "id".to_string(),
        op: FilterOp::Equals(id.to_string()),
    }));

    let locale_ctx: Option<&LocaleContext> = None;
    let matched = query::count_with_search(
        conn,
        ctx.slug,
        def,
        &filters,
        locale_ctx,
        None,
        include_deleted,
    )?;

    if matched == 0 {
        return Err(ServiceError::AccessDenied(format!(
            "{operation} access denied"
        )));
    }

    Ok(())
}

/// Rewrite a list read's dotted group sort (`seo.title`, `-seo.title`) to the
/// column it sorts by (`seo__title`) — the chokepoint every list read
/// (`find_documents`, `search_documents`) passes, whichever surface or
/// internal caller built the query — so the cursor encodes, and the sort-locale
/// check reads, the column the SQL orders by.
pub(crate) fn normalize_sort(fq: &mut FindQuery, fields: &[FieldDefinition]) {
    fq.order_by = fq
        .order_by
        .as_deref()
        .map(|order| normalize_order_by(order, fields));
}

/// Inputs for [`build_pagination`]. Grouped into a struct per
/// CLAUDE.md's "more than 4 parameters" rule; constructed at the two
/// call sites in the read service (`find_documents`,
/// `search_documents`).
pub(crate) struct PaginationInputs<'a> {
    pub docs: &'a [Document],
    pub total: i64,
    pub fq: &'a FindQuery,
    /// The collection's fields and the read's locale context: an all-locales
    /// read sorted by a localized column holds its per-locale map, and the
    /// cursor records the locale SQL orders by.
    pub fields: &'a [FieldDefinition],
    pub locale_ctx: Option<&'a LocaleContext>,
    pub cursor_enabled: bool,
    pub has_timestamps: bool,
    /// Whether the collection has drafts enabled — controls cursor
    /// `status_val` encoding for the composite ordering surfaced by
    /// `apply_order_by`.
    pub has_drafts: bool,
    pub cursor_has_more: Option<bool>,
}

/// Build a `PaginationResult` from query state, supporting both cursor and page modes.
///
/// Shared by `find_documents` and `search_documents` to avoid duplicating the
/// cursor/page branching logic.
pub(crate) fn build_pagination(inputs: &PaginationInputs<'_>) -> query::PaginationResult {
    let limit = inputs.fq.limit.unwrap_or(inputs.total);

    if inputs.cursor_enabled {
        let order_by = inputs.fq.order_by.as_deref();
        let sort_locale = query::cursor_sort_locale(
            order_by,
            inputs.has_timestamps,
            inputs.fields,
            inputs.locale_ctx,
        );

        query::PaginationResult::builder(inputs.docs, inputs.total, limit).cursor(
            order_by,
            query::CursorFlags {
                has_timestamps: inputs.has_timestamps,
                has_drafts: inputs.has_drafts,
                had_before_cursor: inputs.fq.before_cursor.is_some(),
                had_any_cursor: inputs.fq.after_cursor.is_some()
                    || inputs.fq.before_cursor.is_some(),
                cursor_has_more: inputs.cursor_has_more,
                sort_locale,
            },
        )
    } else {
        let offset = inputs.fq.offset.unwrap_or(0);
        let page = if limit > 0 { offset / limit + 1 } else { 1 };
        query::PaginationResult::builder(inputs.docs, inputs.total, limit).page(page, offset)
    }
}

/// Bump the query limit by one when keyset (cursor) pagination must peek at the
/// next row to decide `has_more`. Returns whether overfetch is active — the
/// caller passes that flag back to [`finish_cursor_overfetch`] after fetching.
/// Shared by `find_documents` and `search_documents`.
pub(crate) fn begin_cursor_overfetch(fq: &mut FindQuery, cursor_enabled: bool) -> bool {
    let had_cursor = fq.after_cursor.is_some() || fq.before_cursor.is_some();
    let overfetch = cursor_enabled && had_cursor;

    if overfetch {
        fq.limit = fq.limit.map(|l| l + 1);
    }

    overfetch
}

/// Undo [`begin_cursor_overfetch`]'s limit bump and trim the peeked extra row
/// (the first row when paging backward, else the last). Returns `Some(has_more)`
/// when cursor pagination is active, else `None`. Shared by `find_documents` and
/// `search_documents`.
pub(crate) fn finish_cursor_overfetch(
    fq: &mut FindQuery,
    docs: &mut Vec<Document>,
    overfetch: bool,
    total: i64,
) -> Option<bool> {
    // Restore the original limit for pagination math.
    if overfetch {
        fq.limit = fq.limit.map(|l| l - 1);
    }

    if !overfetch {
        return None;
    }

    let limit = fq.limit.unwrap_or(total);

    // Saturate the doc count for the unreachable case so the `>` check holds.
    let docs_count = i64::try_from(docs.len()).unwrap_or(i64::MAX);
    if docs_count <= limit {
        return Some(false);
    }

    if fq.before_cursor.is_some() {
        docs.remove(0);
    } else {
        docs.pop();
    }

    Some(true)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::{
        core::{
            CollectionDefinition, DocumentFields, FieldAdmin, FieldType,
            upload::{CollectionUpload, ImageSize},
        },
        hooks::{
            AccessCheckInput, ValidationCtx, lifecycle::operation::COLLECTION_WRITE_OPERATIONS,
        },
    };

    /// The operation a state write names to its hooks is one the typed hook
    /// contexts declare — `crap.hook.<Slug>.operation` is built from
    /// `COLLECTION_WRITE_OPERATIONS`.
    #[test]
    fn state_change_operations_are_declared_write_operations() {
        for change in [StateChange::Unpublish, StateChange::Undelete] {
            assert!(
                COLLECTION_WRITE_OPERATIONS.contains(&change.operation()),
                "{change:?} names `{}`",
                change.operation()
            );
        }
    }

    /// Write hooks that run nothing.
    struct NoWriteHooks;

    impl WriteHooks for NoWriteHooks {
        fn run_before_write(
            &self,
            _: &Hooks,
            _: &[FieldDefinition],
            ctx: HookContext,
            _: &ValidationCtx,
        ) -> anyhow::Result<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _: &Hooks,
            _: &[FieldDefinition],
            _: HookEvent,
            ctx: HookContext,
            _: &dyn DbConnection,
        ) -> anyhow::Result<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _: &Hooks,
            _: HookEvent,
            ctx: HookContext,
            _: &dyn DbConnection,
        ) -> anyhow::Result<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _: &AccessCheckInput<'_>) -> anyhow::Result<AccessResult> {
            Ok(AccessResult::Allowed)
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

    impl FieldReadStrip for NoWriteHooks {}

    /// A read strip that drops `title` and records whether the hidden `secret`
    /// was still present when it ran, plus how many batch calls it saw.
    #[derive(Default)]
    struct DropTitle {
        saw_secret: Cell<bool>,
        batches: Cell<usize>,
    }

    impl FieldReadStrip for DropTitle {
        fn strip_read_access_doc(
            &self,
            _: &[FieldDefinition],
            doc: &mut Document,
            _: &str,
            _: Option<&Document>,
            _: Option<&str>,
        ) {
            self.saw_secret.set(doc.fields.contains_key("secret"));
            doc.fields.remove("title");
        }

        fn strip_read_access_docs(
            &self,
            _: &[FieldDefinition],
            docs: &mut [Document],
            _: &str,
            _: Option<&Document>,
            _: Option<&str>,
        ) {
            self.batches.set(self.batches.get() + 1);
            self.saw_secret
                .set(docs.iter().all(|d| d.fields.contains_key("secret")));
        }
    }

    fn strip_args(fields: &[FieldDefinition]) -> ReadStripArgs<'_> {
        ReadStripArgs::builder(fields, "posts").build()
    }

    /// Regression: a write reported an upload document without its `sizes`
    /// object — the per-size values a read folds into it came back flat.
    #[test]
    fn a_reported_upload_document_carries_its_sizes() {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload {
            enabled: true,
            image_sizes: vec![ImageSize::builder("thumbnail").width(10).height(10).build()],
            ..Default::default()
        });
        let ctx = ServiceContext::collection("media", &def).build();
        let mut doc = Document::new("m1".to_string());
        doc.fields
            .insert("thumbnail_url".to_string(), json!("/uploads/t.png"));
        doc.fields.insert("thumbnail_width".to_string(), json!(10));
        doc.fields.insert("thumbnail_height".to_string(), json!(10));

        strip_reported(&ctx, &NoWriteHooks, &mut doc, None).unwrap();

        assert!(doc.fields.contains_key("sizes"), "{:?}", doc.fields);
        assert!(!doc.fields.contains_key("thumbnail_url"));
    }

    /// The reported shape folds an upload's sizes and strips nothing: a system
    /// report leaves stripping to per-subscriber event delivery.
    #[test]
    fn the_reported_shape_folds_sizes_without_stripping() {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload {
            enabled: true,
            image_sizes: vec![ImageSize::builder("thumbnail").width(10).height(10).build()],
            ..Default::default()
        });
        def.fields = vec![text_field("secret", true, false)];
        let ctx = ServiceContext::collection("media", &def).build();
        let mut doc = Document::new("m1".to_string());
        doc.fields
            .insert("thumbnail_url".to_string(), json!("/uploads/t.png"));
        doc.fields.insert("secret".to_string(), json!("kept"));

        shape_reported(&ctx, &mut doc);

        assert!(doc.fields.contains_key("sizes"), "{:?}", doc.fields);
        assert!(!doc.fields.contains_key("thumbnail_url"));
        assert_eq!(doc.fields.get("secret"), Some(&json!("kept")));
    }

    // ── validate_password_policy ──────────────────────────────────────

    #[test]
    fn password_policy_non_auth_is_skipped() {
        // A non-auth collection may carry a legitimate `password` field; the
        // policy never applies to it.
        assert!(
            validate_password_policy(false, Some("x"), None, EmptyPassword::IsRejected).is_ok()
        );
    }

    /// An absent password is always fine — an auth document may have none.
    /// An empty one depends on the caller: "no change" on update, an error on
    /// create, where treating it as "no change" would quietly produce a
    /// passwordless account.
    #[test]
    fn password_policy_absent_is_skipped_and_empty_follows_the_caller() {
        assert!(validate_password_policy(true, None, None, EmptyPassword::IsRejected).is_ok());
        assert!(validate_password_policy(true, None, None, EmptyPassword::MeansNoChange).is_ok());

        assert!(
            validate_password_policy(true, Some(""), None, EmptyPassword::MeansNoChange).is_ok()
        );

        let err = validate_password_policy(true, Some(""), None, EmptyPassword::IsRejected)
            .expect_err("an empty password on create is a caller error");
        match err {
            ServiceError::Validation(ve) => assert_eq!(ve.errors[0].field, "password"),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn password_policy_weak_rejected_as_password_field_error() {
        // `None` policy falls back to the default (min length 8): a short
        // password is rejected as a structured `password` field error, so every
        // surface renders it on the password input.
        let err = validate_password_policy(true, Some("short"), None, EmptyPassword::IsRejected)
            .unwrap_err();
        match err {
            ServiceError::Validation(ve) => {
                assert_eq!(ve.errors.len(), 1);
                assert_eq!(ve.errors[0].field, "password");
                assert_eq!(
                    ve.errors[0].key.as_deref(),
                    Some("validation.password_min_length")
                );
                assert_eq!(
                    ve.errors[0].params.get("min").map(String::as_str),
                    Some("8")
                );
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn password_policy_valid_passes() {
        assert!(
            validate_password_policy(true, Some("longenough"), None, EmptyPassword::IsRejected)
                .is_ok()
        );
    }

    #[test]
    fn password_policy_none_falls_back_to_default_never_skips() {
        // The fail-safe: a context that forgets to thread a policy still enforces
        // the DEFAULT policy — never no enforcement.
        assert!(
            validate_password_policy(true, Some("weak"), None, EmptyPassword::IsRejected).is_err()
        );
    }

    #[test]
    fn password_policy_uses_threaded_policy_over_default() {
        // A stricter configured policy applies when threaded.
        let strict = PasswordPolicy {
            min_length: 12,
            ..PasswordPolicy::default()
        };
        // Passes default (>=8) but fails the stricter threaded policy (>=12).
        assert!(
            validate_password_policy(
                true,
                Some("longenough"),
                Some(&strict),
                EmptyPassword::IsRejected
            )
            .is_err()
        );
    }

    /// Helper: build a Text field with the given hidden flags.
    fn text_field(name: &str, hidden: bool, admin_hidden: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .hidden(hidden)
            .admin(FieldAdmin::builder().hidden(admin_hidden).build())
            .build()
    }

    /// Top-level `hidden = true` → field is collected for API stripping.
    #[test]
    fn collects_top_level_hidden_field() {
        let fields = vec![text_field("secret", true, false)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert_eq!(names, vec![FieldDenial::Flat("secret".into())]);
    }

    /// `admin.hidden = true` (only) → NOT collected. This is the upload-bug
    /// fix: `admin.hidden` is a rendering flag, not a data-visibility flag.
    #[test]
    fn does_not_collect_admin_hidden_only() {
        let fields = vec![text_field("url", false, true)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert!(
            names.is_empty(),
            "admin.hidden alone must not strip from API responses"
        );
    }

    /// Both flags set → still collected (top-level wins; admin.hidden is redundant but legal).
    #[test]
    fn collects_when_both_flags_set() {
        let fields = vec![text_field("internal", true, true)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert_eq!(names, vec![FieldDenial::Flat("internal".into())]);
    }

    /// Default field (neither flag) → not collected.
    #[test]
    fn does_not_collect_visible_field() {
        let fields = vec![text_field("title", false, false)];

        let names = collect_api_hidden_field_names(&fields, "");

        assert!(names.is_empty());
    }

    /// Group with `hidden = true` parent → parent name returned, subfields skipped
    /// (parent-hidden short-circuit preserved from the original implementation).
    #[test]
    fn hidden_group_parent_skips_subfields() {
        let group = FieldDefinition::builder("meta", FieldType::Group)
            .hidden(true)
            .fields(vec![text_field("inner", false, false)])
            .build();

        let names = collect_api_hidden_field_names(&[group], "");

        assert_eq!(names, vec![FieldDenial::Flat("meta".into())]);
    }

    /// Group with visible parent but hidden subfield → subfield collected with
    /// `parent__child` prefix.
    #[test]
    fn visible_group_collects_hidden_subfields_with_prefix() {
        let group = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                text_field("title", false, false),
                text_field("internal_score", true, false),
            ])
            .build();

        let names = collect_api_hidden_field_names(&[group], "");

        assert_eq!(names, vec![FieldDenial::Flat("seo__internal_score".into())]);
    }

    /// A document with a visible `title` and a hidden `secret`.
    fn doc_with_secret(id: &str) -> Document {
        let mut doc = Document::new(id.to_string());
        doc.fields.insert("title".into(), json!("Hello"));
        doc.fields.insert("secret".into(), json!("s"));
        doc
    }

    /// The read-access strip judges the document with its hidden fields still
    /// present; the hidden fields are stripped after it.
    #[test]
    fn strip_unreadable_strips_read_denied_before_hidden_fields() {
        let fields = vec![
            text_field("title", false, false),
            text_field("secret", true, false),
        ];
        let mut doc = doc_with_secret("d1");
        let strip = DropTitle::default();

        strip_unreadable(&strip, &strip_args(&fields), &mut doc);

        assert!(
            strip.saw_secret.get(),
            "the read-access strip sees hidden fields"
        );
        assert!(doc.fields.is_empty(), "{:?}", doc.fields);
    }

    /// The map form runs the given read strip first, then removes the hidden
    /// fields — the shape a live event's payload is stripped in.
    #[test]
    fn strip_unreadable_fields_strips_hidden_after_the_read_strip() {
        let fields = vec![
            text_field("title", false, false),
            text_field("secret", true, false),
        ];
        let mut level: Map<String, Value> = Map::new();
        level.insert("title".into(), json!("Hello"));
        level.insert("secret".into(), json!("s"));
        level.insert("token".into(), json!("t"));
        let mut saw_secret = false;

        strip_unreadable_fields(&fields, &mut level, |level| {
            saw_secret = level.contains_key("secret");
            level.remove("token");
        });

        assert!(saw_secret, "the read-access strip sees hidden fields");
        assert_eq!(Value::Object(level), json!({ "title": "Hello" }));
    }

    /// The batch strip runs the read-access strip once over every document, then
    /// strips the hidden fields from each.
    #[test]
    fn strip_unreadable_docs_strips_the_whole_batch() {
        let fields = vec![
            text_field("title", false, false),
            text_field("secret", true, false),
        ];
        let mut docs = vec![doc_with_secret("d1"), doc_with_secret("d2")];
        let strip = DropTitle::default();

        strip_unreadable_docs(&strip, &strip_args(&fields), &mut docs);

        assert!(strip.saw_secret.get(), "the batch sees hidden fields");
        assert_eq!(strip.batches.get(), 1);
        for doc in &docs {
            assert!(!doc.fields.contains_key("secret"), "{:?}", doc.fields);
            assert_eq!(doc.fields.get("title"), Some(&json!("Hello")));
        }
    }
}
