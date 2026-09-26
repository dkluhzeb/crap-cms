//! The gate every document update passes before its before-write hooks run:
//! the document row is locked, the admission prefix (canonicalize, adopt the
//! pending draft as the write's base, locale lock) runs, the `update` access
//! rule is applied, the revision is checked against the caller's
//! `expected_revision` and moved forward, and write-denied fields are stripped
//! from the request and from the draft it publishes. The request also keeps
//! every stored value its writer cannot read: a write never changes a value
//! its writer cannot see. The single-document update and the bulk update share
//! it, so a rule enforced on one cannot be missing on the other.

use serde_json::Value;

use crate::{
    core::DocumentFields,
    db::{DbConnection, LocaleContext, query::StoredRow},
    service::{
        ServiceContext, ServiceError, UpdateStored, WriteInput,
        write::{
            PublishStored, admit_update_input, check_update_access, claim_revision,
            draft_save_base, stored_fields_for_update_rules,
        },
    },
};

/// Strip write-denied fields before hook processing (data-aware: each
/// `access.update` rule sees `ctx.data` = its level and `ctx.document` = the
/// stored document, never the patch it is judging), and keep every value the
/// writer cannot read as it is (each `access.read` rule judges what the write
/// replaces: the pending draft for a draft save, else the stored document), so
/// a blank the writer could not see past never lands.
///
/// Returns the stored document the rules judged.
///
/// # Errors
///
/// Returns a backend error if the stored document or the pending draft cannot
/// be read.
fn strip_update_input(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    input: &mut WriteInput<'_>,
) -> Result<DocumentFields, ServiceError> {
    let def = ctx.collection_def()?;

    let stored = stored_fields_for_update_rules(conn, ctx.slug, def, id, input.locale_ctx)?;
    let draft = draft_save_base(
        conn,
        &StoredRow {
            table: ctx.slug,
            id,
            fields: &def.fields,
            locale_ctx: input.locale_ctx,
        },
        input.draft && def.has_drafts() && def.has_versions(),
    )?;

    ctx.write_hooks()?.strip_write_access_update(
        &def.fields,
        &mut input.data,
        UpdateStored::new(&stored, draft.as_ref().unwrap_or(&stored)),
        ctx.slug,
        ctx.user,
        input.locale_ctx.map(LocaleContext::access_locale),
    );

    Ok(stored)
}

/// Admit an update: lock the document row, run the admission prefix
/// (canonicalize, adopt the pending draft, locale lock), apply the `update`
/// access rule, then strip write-denied fields.
///
/// Returns the drafted snapshot the publish writes back — already stripped by
/// the publisher's field-level write access — or `None` when no draft is
/// pending or this is not a publish.
///
/// # Errors
///
/// Returns a backend error if the row cannot be locked, the locale-lock
/// validation error, or the access denial.
pub(super) fn admit_update(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    id: &str,
    input: &mut WriteInput<'_>,
) -> Result<Option<Value>, ServiceError> {
    // Held from here to commit: every read this write builds on (the pending
    // draft, the stored row the access rules judge, the files it may drop)
    // must see the same state the write lands on, and the before-write hooks
    // run inside it. A hook that writes a second document therefore takes
    // that row's lock while holding this one; Postgres breaks such a cycle by
    // aborting one side with a retryable error.
    let def = ctx.collection_def()?;
    let write_hooks = ctx.write_hooks()?;

    // Serialize concurrent writers of this document before the write reads
    // anything it builds on. The pending draft below is read with a plain
    // SELECT, so without the lock a publisher can adopt a snapshot a
    // concurrent draft save has already superseded and write that stale
    // content back as the published version, burying the newer draft. Every
    // later read of this document — the stored row the access rules judge, the
    // files the write may drop, the outgoing-ref snapshot at persist time —
    // sits behind the same lock. No-op on SQLite, whose IMMEDIATE transaction
    // serializes writers already.
    conn.lock_row(ctx.slug, id)?;

    // The same prefix the `validate` dry-run runs, so the two judge one input.
    // Its refusal (a locale-locked field) is raised only past the access
    // gate, its non-object groups only past the field-level write strip: a
    // caller without access learns nothing about the schema from them.
    let admission = admit_update_input(ctx, def, id, input)?;

    check_update_access(
        ctx,
        write_hooks,
        def,
        id,
        &input.data,
        input.locale_ctx.map(LocaleContext::access_locale),
    )?;

    let (pending_draft, groups) = admission.admit()?;

    // Checked under the lock and only for an admitted writer: a caller the
    // access rule refuses learns nothing about the document's revision. The
    // bump lands in this transaction, so a write that fails later leaves the
    // revision where it was.
    claim_revision(conn, ctx.slug, id, input.expected_revision)?;

    let stored = strip_update_input(ctx, conn, id, input)?;

    // A non-object group the strip left is refused; one it dropped (its
    // writer may not write or read it) is silent, like any other such field.
    groups.refuse_unstripped(&input.data)?;

    // The write-back judges each locale against the row as that locale
    // stores it.
    let load = |locale_ctx: Option<&LocaleContext>| {
        stored_fields_for_update_rules(conn, ctx.slug, def, id, locale_ctx)
    };

    pending_draft.publishing_snapshot(
        ctx,
        write_hooks,
        PublishStored::new(&stored, &load),
        input.locale_ctx,
    )
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use anyhow::Result as AnyResult;
    use rusqlite::Connection;
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{
            CollectionDefinition, Document, DocumentFields, FieldAccess, FieldDefinition,
            FieldType, HookRef, Hooks, ValidationError, VersionsConfig,
        },
        db::{AccessResult, LocaleMode, query, query::test_helpers::CountingConn},
        hooks::{
            AccessCheckInput, HookContext, HookEvent, ValidationCtx,
            lifecycle::access::strip_read_access_data_aware,
        },
        service::{FieldReadStrip, WriteHooks},
    };

    /// Write hooks that run nothing and allow every access check but the field
    /// `access.read` rules named `deny` (always), `deny_de` (in `de`) and
    /// `deny_when_private` (on a level whose `private` is set).
    struct NoopWriteHooks;

    impl WriteHooks for NoopWriteHooks {
        fn run_before_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            ctx: HookContext,
            _val_ctx: &ValidationCtx,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_after_write(
            &self,
            _hooks: &Hooks,
            _fields: &[FieldDefinition],
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn run_hooks_with_conn(
            &self,
            _hooks: &Hooks,
            _event: HookEvent,
            ctx: HookContext,
            _conn: &dyn DbConnection,
        ) -> AnyResult<HookContext> {
            Ok(ctx)
        }

        fn check_access(&self, _input: &AccessCheckInput<'_>) -> AnyResult<AccessResult> {
            Ok(AccessResult::Allowed)
        }

        fn validate_fields(
            &self,
            _fields: &[FieldDefinition],
            _data: &DocumentFields,
            _ctx: &ValidationCtx,
        ) -> std::result::Result<(), ValidationError> {
            Ok(())
        }
    }

    impl FieldReadStrip for NoopWriteHooks {
        fn strip_read_access_map(
            &self,
            fields: &[FieldDefinition],
            level: &mut Map<String, Value>,
            _document: &DocumentFields,
            _collection: &str,
            _user: Option<&Document>,
            locale: Option<&str>,
        ) {
            let is_denied = |hook: &HookRef, data: &DocumentFields| match hook.reference() {
                "deny" => true,
                "deny_de" => locale == Some("de"),
                "deny_when_private" => data
                    .get("private")
                    .is_some_and(|v| *v == json!(true) || *v == json!(1)),
                _ => false,
            };

            strip_read_access_data_aware(fields, level, &is_denied);
        }
    }

    /// A versioned, draft-enabled `posts` collection with one scalar field.
    fn posts() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.versions = Some(VersionsConfig::new(true, 10));
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        def
    }

    /// A published `posts` row with a newer draft pending on it.
    fn drafted_posts() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                _revision INTEGER NOT NULL DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT,
                created_at TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Published');",
        )
        .unwrap();

        let drafted = json!({ "title": "Drafted" });
        query::create_version(&conn, "posts", "p1", "draft", &drafted).unwrap();

        conn
    }

    /// The document row is locked before the publish reads the pending draft
    /// it is about to make live. The draft is read with a plain SELECT, so an
    /// unlocked publisher could build its published version from a snapshot a
    /// concurrent draft save had already superseded and bury that newer draft.
    #[test]
    fn a_publish_locks_the_row_before_it_reads_the_pending_draft() {
        let conn = drafted_posts();
        let spy = CountingConn::new(&conn);
        let def = posts();
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&spy)
            .write_hooks(&hooks)
            .build();

        let mut input = WriteInput::builder(DocumentFields::new()).build();

        let adopted = admit_update(&ctx, &spy, "p1", &mut input).unwrap();

        assert!(adopted.is_some(), "the publish adopted the pending draft");
        assert_eq!(input.data.get("title"), Some(&json!("Drafted")));
        assert_eq!(spy.locks(), vec![("posts".to_string(), "p1".to_string())]);
        assert_eq!(
            spy.reads_at_locks(),
            vec![0],
            "the row lock is taken before the version-table read, not after it"
        );
        assert!(
            spy.reads() > 0,
            "the draft snapshot was read under the lock"
        );
    }

    /// The lock is not conditional on there being a draft to adopt: a draft
    /// save contends with the publish that reads its snapshot, so it takes the
    /// same lock.
    #[test]
    fn a_draft_save_locks_the_row_too() {
        let conn = drafted_posts();
        let spy = CountingConn::new(&conn);
        let def = posts();
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&spy)
            .write_hooks(&hooks)
            .build();

        let mut input = WriteInput::builder(DocumentFields::new())
            .draft(true)
            .build();

        let adopted = admit_update(&ctx, &spy, "p1", &mut input).unwrap();

        assert!(adopted.is_none(), "a draft save publishes nothing");
        assert_eq!(spy.locks(), vec![("posts".to_string(), "p1".to_string())]);
        assert_eq!(spy.reads_at_locks(), vec![0]);
    }

    /// An update carrying the revision its writer read is admitted and moves
    /// the revision forward; a second one carrying that same, now stale,
    /// revision is refused with a conflict before any hook runs, leaving the
    /// revision where the first write put it. A draft save counts like any
    /// other write.
    #[test]
    fn an_update_is_admitted_only_at_the_revision_its_writer_read() {
        let conn = drafted_posts();
        let def = posts();
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        let loaded = query::read_revision(&conn, "posts", "p1").unwrap();
        assert_eq!(loaded, Some(0));

        let mut first = WriteInput::builder(DocumentFields::new())
            .draft(true)
            .expected_revision(loaded)
            .build();
        admit_update(&ctx, &conn, "p1", &mut first).unwrap();

        assert_eq!(query::read_revision(&conn, "posts", "p1").unwrap(), Some(1));

        let mut second = WriteInput::builder(DocumentFields::new())
            .expected_revision(loaded)
            .build();
        let err = admit_update(&ctx, &conn, "p1", &mut second).unwrap_err();

        assert!(matches!(err, ServiceError::Conflict(_)), "{err:?}");
        assert_eq!(query::read_revision(&conn, "posts", "p1").unwrap(), Some(1));
    }

    fn read_denied(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, field_type)
            .access(FieldAccess {
                read: Some("deny".into()),
                ..Default::default()
            })
            .build()
    }

    /// Regression: saving the admin edit form as a user who may update but
    /// not read a field overwrote it — the form submits an unchecked box and
    /// an empty input for what it could not show. The update keeps every
    /// value its writer cannot read, top-level and inside a group alike.
    #[test]
    fn an_update_keeps_the_values_its_writer_cannot_read() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                verified INTEGER,
                internal__label TEXT,
                internal__note TEXT,
                _revision INTEGER NOT NULL DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, title, verified, internal__label, internal__note)
            VALUES ('p1', 'Old', 1, 'a', 'secret');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            read_denied("verified", FieldType::Checkbox),
            FieldDefinition::builder("internal", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    read_denied("note", FieldType::Text),
                ])
                .build(),
        ];

        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        let submitted: DocumentFields = [
            ("title".to_string(), json!("New")),
            ("verified".to_string(), json!(false)),
            ("internal".to_string(), json!({ "label": "b", "note": "" })),
        ]
        .into_iter()
        .collect();
        let mut input = WriteInput::builder(submitted).build();

        admit_update(&ctx, &conn, "p1", &mut input).unwrap();

        assert_eq!(input.data.get("title"), Some(&json!("New")));
        assert_eq!(
            input.data.get("verified"),
            Some(&json!(true)),
            "an unreadable checkbox keeps its stored value"
        );
        assert_eq!(
            input.data.get("internal"),
            Some(&json!({ "label": "b" })),
            "an unreadable group sub-field is left out of the write"
        );
    }

    fn read_gated(name: &str, field_type: FieldType, rule: &str) -> FieldDefinition {
        FieldDefinition::builder(name, field_type)
            .access(FieldAccess {
                read: Some(rule.into()),
                ..Default::default()
            })
            .build()
    }

    /// A drafted `posts` collection whose `note` its writer cannot read while
    /// the document is `private`.
    fn private_posts(
        published_private: bool,
        drafted_private: bool,
    ) -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                private INTEGER,
                note TEXT,
                _status TEXT DEFAULT 'published',
                _revision INTEGER NOT NULL DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT,
                created_at TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts (id, title, private, note) VALUES ('p1', 'T', ?1, 'live note')",
            [i64::from(published_private)],
        )
        .unwrap();

        let drafted = json!({ "title": "T", "private": drafted_private, "note": "drafted note" });
        query::create_version(&conn, "posts", "p1", "draft", &drafted).unwrap();

        let mut def = posts();
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("private", FieldType::Checkbox).build(),
            read_gated("note", FieldType::Text, "deny_when_private"),
        ];

        (conn, def)
    }

    /// Run a draft save sending `note` over the `private_posts` fixture.
    fn draft_save_note(published_private: bool, drafted_private: bool) -> DocumentFields {
        let (conn, def) = private_posts(published_private, drafted_private);
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();

        let sent: DocumentFields = [("note".to_string(), json!("sent"))].into_iter().collect();
        let mut input = WriteInput::builder(sent).draft(true).build();

        admit_update(&ctx, &conn, "p1", &mut input).unwrap();

        input.data
    }

    /// Regression: a draft save judged the values its writer cannot read
    /// against the published row, while its form showed the pending draft. A
    /// value the draft hides was overwritten blind, and a value only the
    /// published row hides could not be edited in the draft at all.
    #[test]
    fn a_draft_save_judges_what_its_writer_cannot_read_against_the_pending_draft() {
        let hidden_in_draft = draft_save_note(false, true);
        assert!(
            !hidden_in_draft.contains_key("note"),
            "the draft hides the note, so the save keeps it: {hidden_in_draft:?}"
        );

        let hidden_when_published = draft_save_note(true, false);
        assert_eq!(
            hidden_when_published.get("note"),
            Some(&json!("sent")),
            "the draft shows the note, so the save changes it"
        );
    }

    /// A drafted collection with a shared `title` and a `note` localized in
    /// `en`/`de` that its writer cannot read in `de`.
    fn localized_posts() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                note__en TEXT,
                note__de TEXT,
                _status TEXT DEFAULT 'published',
                _revision INTEGER NOT NULL DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT,
                created_at TEXT
            );
            INSERT INTO posts (id, title, note__en, note__de)
            VALUES ('p1', 'T', 'live en', 'live de');",
        )
        .unwrap();

        let drafted = json!({
            "title": "T2",
            "note": "draft en",
            "note__en": "draft en",
            "note__de": "draft de",
        });
        query::create_version(&conn, "posts", "p1", "draft", &drafted).unwrap();

        let mut def = posts();
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("note", FieldType::Text)
                .localized(true)
                .access(FieldAccess {
                    read: Some("deny_de".into()),
                    ..Default::default()
                })
                .build(),
        ];

        (conn, def)
    }

    /// Publish the `localized_posts` draft under `locale`, returning the
    /// request data and the snapshot written back.
    fn publish_in(locale: &str) -> (DocumentFields, Value) {
        let (conn, def) = localized_posts();
        let hooks = NoopWriteHooks;
        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .write_hooks(&hooks)
            .build();
        let locale_ctx = LocaleContext {
            mode: LocaleMode::Single(locale.to_string()),
            config: LocaleConfig {
                default_locale: "en".to_string(),
                locales: vec!["en".to_string(), "de".to_string()],
                fallback: true,
            },
        };

        let mut input = WriteInput::builder(DocumentFields::new())
            .locale_ctx(Some(&locale_ctx))
            .build();
        let snapshot = admit_update(&ctx, &conn, "p1", &mut input)
            .unwrap()
            .expect("the publish adopts the pending draft");

        (input.data, snapshot)
    }

    /// Regression: publishing a pending draft wrote every locale of it back
    /// over the row, including a drafted value its publisher cannot read in
    /// that locale. The write-back is judged per locale: the `de` note keeps
    /// its stored value, the `en` one goes live — whichever locale publishes.
    #[test]
    fn a_publish_keeps_each_locales_values_its_publisher_cannot_read() {
        let (data, snapshot) = publish_in("en");
        assert_eq!(data.get("note"), Some(&json!("draft en")));
        assert_eq!(snapshot.get("note__en"), Some(&json!("draft en")));
        assert!(snapshot.get("note__de").is_none(), "{snapshot}");

        let (data, snapshot) = publish_in("de");
        assert!(!data.contains_key("note"), "{data:?}");
        assert_eq!(snapshot.get("note__en"), Some(&json!("draft en")));
        assert!(snapshot.get("note__de").is_none(), "{snapshot}");
        assert_eq!(snapshot.get("title"), Some(&json!("T2")));
    }
}
