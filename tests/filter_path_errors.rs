//! A filter naming a field or path the collection does not have is the
//! caller's mistake: every service operation that filters — find, count and
//! the bulk writes — answers a typed validation error naming the path, never
//! an internal error.

#![allow(clippy::missing_panics_doc)]

use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{
    DbConnection, DbPool, Filter, FilterClause, FilterOp, FindQuery, migrate, pool,
    query::cursor::{CursorData, SortValue},
};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    CountDocumentsInput, DeleteManyOptions, FindDocumentsInput, RunnerReadHooks, ServiceContext,
    ServiceError, UpdateManyOptions, count_documents, delete_many, find_documents, update_many,
};
use serde_json::json;

/// Paths the `posts` collection does not have: an unknown field, a sub-path
/// into a text field, an unknown array sub-field, a sub-path into an array
/// sub-field that is not a group.
const BAD_PATHS: [&str; 4] = ["nope", "title.sub", "items.nope", "items.name.deep"];

struct Setup {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: Arc<CollectionDefinition>,
}

/// `posts` with a `title`, an `items` array whose rows hold a `name`, and an
/// `seo` group holding a `title`.
fn setup() -> Setup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("name", FieldType::Text).build(),
            ])
            .build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
            ])
            .build(),
    ];

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    Setup {
        _tmp: tmp,
        pool,
        runner,
        def: Arc::new(def),
    }
}

fn filters(path: &str) -> Vec<FilterClause> {
    vec![FilterClause::Single(Filter {
        field: path.to_string(),
        op: FilterOp::Equals("x".to_string()),
    })]
}

/// Assert `err` is a validation error naming `path`.
fn assert_names_path(op: &str, path: &str, err: &ServiceError) {
    let ServiceError::Validation(ve) = err else {
        panic!("{op} {path}: expected a validation error, got {err:?}");
    };

    assert_eq!(ve.errors[0].field, path, "{op}: {ve}");
}

#[test]
fn find_and_count_reject_a_bad_filter_path_as_validation() {
    let s = setup();
    let conn = s.pool.get().expect("conn");
    let hooks = RunnerReadHooks::new(&s.runner, &conn, None, None);
    let ctx = ServiceContext::collection("posts", &s.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    for path in BAD_PATHS {
        let filters = filters(path);
        let query = FindQuery::builder().filters(filters.clone()).build();

        let Err(find) = find_documents(&ctx, &FindDocumentsInput::builder(&query).build()) else {
            panic!("find must reject the path {path}");
        };
        let count = count_documents(&ctx, &CountDocumentsInput::builder(&filters).build())
            .expect_err("count must reject the path");

        assert_names_path("find", path, &find);
        assert_names_path("count", path, &count);
    }
}

#[test]
fn bulk_writes_reject_a_bad_filter_path_as_validation() {
    let s = setup();
    let ctx = ServiceContext::collection("posts", &s.def)
        .pool(&s.pool)
        .runner(&s.runner)
        .override_access(true)
        .build();

    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!("changed"));

    let update_opts = UpdateManyOptions::builder().run_hooks(false).build();
    let delete_opts = DeleteManyOptions {
        run_hooks: false,
        ..Default::default()
    };
    let locale = LocaleConfig::default();

    for path in BAD_PATHS {
        let filters = filters(path);

        let update = update_many(&ctx, &filters, &data, &locale, &update_opts)
            .expect_err("update_many must reject the path");
        let delete = delete_many(&ctx, &filters, &locale, &delete_opts)
            .expect_err("delete_many must reject the path");

        assert_names_path("update_many", path, &update);
        assert_names_path("delete_many", path, &delete);
    }
}

/// Regression: a dotted group sort (`seo.title`) was rewritten to its column
/// only by the `find` operation, so a read calling `find_documents` directly
/// encoded its cursor for `seo.title` — a name no document holds — and every
/// cursor carried a NULL sort value. Every list read normalizes the sort.
#[test]
fn a_dotted_group_sort_pages_by_its_column_on_every_read() {
    let s = setup();
    let conn = s.pool.get().expect("conn");
    conn.execute_batch(
        "INSERT INTO posts (id, title, seo__title) VALUES ('a', 'x', 'Alpha'), ('b', 'y', 'Beta')",
    )
    .expect("seed");

    let hooks = RunnerReadHooks::new(&s.runner, &conn, None, None);
    let ctx = ServiceContext::collection("posts", &s.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();
    let query = FindQuery::builder()
        .order_by(Some("seo.title".to_string()))
        .limit(Some(1))
        .build();

    let page = find_documents(
        &ctx,
        &FindDocumentsInput::builder(&query)
            .cursor_enabled(true)
            .build(),
    )
    .expect("find");

    let end = page.pagination.end_cursor.expect("an end cursor");
    let cursor = CursorData::decode(&end).expect("decodes");

    assert_eq!(cursor.sort_col, "seo__title");
    assert_eq!(cursor.sort_val, SortValue::Text("Alpha".to_string()));
}
