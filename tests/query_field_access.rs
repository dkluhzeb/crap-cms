//! Filters, sorts, and searches never act as a read-access oracle: a field the
//! caller cannot read (API-hidden, or denied by its `access.read` rule) cannot
//! be filtered or sorted on through any read surface — nor filtered on by a
//! bulk write, whose match counts answer the same question.

use std::path::PathBuf;
use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldAccess, FieldDefinition, FieldType};
use crap_cms::core::{Document, DocumentFields, HookRef, Registry};
use crap_cms::db::{DbPool, Filter, FilterClause, FilterOp, FindQuery, migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    CountDocumentsInput, DeleteManyOptions, FindDocumentsInput, RunnerReadHooks,
    SearchDocumentsInput, ServiceContext, ServiceError, UpdateManyOptions, count_documents,
    delete_many, find_documents, search_documents, update_many,
};
use serde_json::json;

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
}

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("vault");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("secret", FieldType::Text)
            .hidden(true)
            .build(),
        FieldDefinition::builder("notes", FieldType::Textarea)
            .access(FieldAccess {
                read: Some(HookRef::new("access.admin_only")),
                ..Default::default()
            })
            .build(),
    ];
    def
}

fn setup() -> Harness {
    // The example config dir supplies the `access.admin_only` hook module.
    let config_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("example");
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let def = make_def();
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    Harness {
        _tmp: tmp,
        pool,
        runner,
        def,
    }
}

fn admin() -> Document {
    let mut doc = Document::new("admin-1".to_string());
    doc.fields.insert("role".into(), json!("admin"));
    doc
}

fn eq_filter(field: &str) -> Vec<FilterClause> {
    vec![FilterClause::Single(Filter {
        field: field.to_string(),
        op: FilterOp::Equals("x".to_string()),
    })]
}

/// Run `find` with the given filters/sort as `user`, returning the error.
fn find_as(
    h: &Harness,
    user: Option<&Document>,
    filters: Vec<FilterClause>,
    order_by: Option<&str>,
) -> Result<(), ServiceError> {
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, user, None);
    let ctx = ServiceContext::collection("vault", &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .user(user)
        .build();
    let fq = FindQuery::builder()
        .filters(filters)
        .order_by(order_by.map(str::to_string))
        .build();
    let input = FindDocumentsInput::builder(&fq).build();

    find_documents(&ctx, &input).map(|_| ())
}

fn assert_denied(result: Result<(), ServiceError>, field: &str) {
    match result {
        Err(ServiceError::AccessDenied(msg)) => {
            assert!(msg.contains(field), "message should name '{field}': {msg}");
        }
        other => panic!("expected AccessDenied on '{field}', got {other:?}"),
    }
}

#[test]
fn filtering_on_a_hidden_field_is_denied_for_everyone() {
    let h = setup();

    assert_denied(find_as(&h, None, eq_filter("secret"), None), "secret");
    assert_denied(
        find_as(&h, Some(&admin()), eq_filter("secret"), None),
        "secret",
    );
}

#[test]
fn sorting_on_a_hidden_field_is_denied() {
    let h = setup();

    assert_denied(
        find_as(&h, Some(&admin()), vec![], Some("-secret")),
        "secret",
    );
}

#[test]
fn a_hidden_field_inside_an_or_group_is_denied() {
    let h = setup();
    let filters = vec![FilterClause::Or(vec![
        FilterClause::Single(Filter {
            field: "title".into(),
            op: FilterOp::Equals("x".into()),
        }),
        FilterClause::Single(Filter {
            field: "secret".into(),
            op: FilterOp::Equals("x".into()),
        }),
    ])];

    assert_denied(find_as(&h, Some(&admin()), filters, None), "secret");
}

/// A read-gated field follows its rule: denied for an anonymous caller,
/// allowed for an admin — and an unrelated field is always fine.
#[test]
fn read_gated_field_follows_the_read_rule() {
    let h = setup();

    assert_denied(find_as(&h, None, eq_filter("notes"), None), "notes");
    assert_denied(find_as(&h, None, vec![], Some("notes")), "notes");

    find_as(&h, Some(&admin()), eq_filter("notes"), Some("-notes")).expect("admin may filter");
    find_as(&h, None, eq_filter("title"), Some("title")).expect("plain field is fine");
}

#[test]
fn count_and_search_apply_the_same_rule() {
    let h = setup();
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, None, None);
    let ctx = ServiceContext::collection("vault", &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .build();

    let filters = eq_filter("notes");
    let count_input = CountDocumentsInput::builder(&filters).build();
    assert_denied(count_documents(&ctx, &count_input).map(|_| ()), "notes");

    let fq = FindQuery::builder().order_by(Some("secret".into())).build();
    let search_input = SearchDocumentsInput {
        query: &fq,
        locale_ctx: None,
        cursor_enabled: false,
        include_drafts: false,
    };
    assert_denied(search_documents(&ctx, &search_input).map(|_| ()), "secret");
}

/// A pool-mode write context for the bulk operations.
fn bulk_ctx<'a>(
    h: &'a Harness,
    user: Option<&'a Document>,
    override_access: bool,
) -> ServiceContext<'a> {
    ServiceContext::collection("vault", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .user(user)
        .override_access(override_access)
        .build()
}

fn update_many_as(
    h: &Harness,
    user: Option<&Document>,
    override_access: bool,
    filters: &[FilterClause],
) -> Result<(), ServiceError> {
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("renamed"));

    let opts = UpdateManyOptions::builder().run_hooks(false).build();

    let ctx = bulk_ctx(h, user, override_access);

    update_many(&ctx, filters, &data, &LocaleConfig::default(), &opts).map(|_| ())
}

fn delete_many_as(
    h: &Harness,
    user: Option<&Document>,
    override_access: bool,
    filters: &[FilterClause],
) -> Result<(), ServiceError> {
    let opts = DeleteManyOptions {
        run_hooks: false,
        ..Default::default()
    };

    let ctx = bulk_ctx(h, user, override_access);

    delete_many(&ctx, filters, &LocaleConfig::default(), &opts).map(|_| ())
}

/// Regression: `update_many` / `delete_many` only validated the filter's
/// syntax, so their `modified` / `deleted` / `skipped` counts and the
/// `bulk_max_documents` "matched N" error binary-searched a read-denied or
/// hidden value. They now refuse the filter exactly as a find does.
#[test]
fn bulk_writes_reject_a_filter_on_an_unreadable_field() {
    let h = setup();

    assert_denied(
        update_many_as(&h, None, false, &eq_filter("notes")),
        "notes",
    );
    assert_denied(
        delete_many_as(&h, None, false, &eq_filter("notes")),
        "notes",
    );

    assert_denied(
        update_many_as(&h, Some(&admin()), false, &eq_filter("secret")),
        "secret",
    );
    assert_denied(
        delete_many_as(&h, Some(&admin()), false, &eq_filter("secret")),
        "secret",
    );
}

/// The bulk check follows the same read rule: a reader the rule allows may
/// filter on the field, and a plain field is always fine.
#[test]
fn bulk_writes_allow_a_filter_the_read_rule_allows() {
    let h = setup();

    update_many_as(&h, Some(&admin()), false, &eq_filter("notes")).expect("admin may filter");
    delete_many_as(&h, Some(&admin()), false, &eq_filter("notes")).expect("admin may filter");

    update_many_as(&h, None, false, &eq_filter("title")).expect("plain field is fine");
    delete_many_as(&h, None, false, &eq_filter("title")).expect("plain field is fine");
}

/// `override_access` (MCP, internal callers) skips the bulk gate entirely,
/// the unreadable-filter check included — as it skips every access rule.
#[test]
fn bulk_writes_under_override_access_are_unaffected() {
    let h = setup();

    update_many_as(&h, None, true, &eq_filter("notes")).expect("override may filter");
    delete_many_as(&h, None, true, &eq_filter("secret")).expect("override may filter");
}
