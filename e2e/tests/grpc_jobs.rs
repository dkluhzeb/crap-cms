//! gRPC e2e: jobs surface (`TriggerJob` / `GetJobRun` / `ListJobRuns`).
//!
//! `ListJobs` is already covered by `grpc_metadata_auth`; this file
//! exercises the three run-oriented RPCs. The scheduler doesn't run
//! in tests, so triggered jobs sit in `_crap_jobs` with
//! `status = "pending"` — that's enough to verify the RPCs reach
//! the DB, return the expected job ID, and surface the run via
//! `GetJobRun` / `ListJobRuns`. Actual execution semantics belong
//! to the scheduler tests in the main crate.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::collections::HashMap;

use crap_cms::api::content::{DataMap, FieldValue, field_value::Kind};
use tonic::{Code, Request, metadata::MetadataValue};

use crap_cms::{
    api::content::{
        CreateRequest, GetJobRunRequest, JobRunStatus, ListJobRunsRequest, ListJobsRequest,
        LoginRequest, TriggerJobRequest, content_api_client::ContentApiClient,
    },
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
        job::JobDefinition,
    },
};
use crap_cms_e2e::spawn_grpc_server_with_jobs;

fn make_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("User".to_string())),
        plural: Some(LocalizedString::Plain("Users".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("name", FieldType::Text).build(),
    ];
    def.auth = Some(Auth::enabled());
    def
}

fn make_test_job() -> JobDefinition {
    JobDefinition::builder("cleanup", "jobs.cleanup.run").build()
}

fn proto_struct(pairs: &[(&str, &str)]) -> DataMap {
    let mut fields = HashMap::new();
    for (k, v) in pairs {
        fields.insert(
            (*k).to_string(),
            FieldValue {
                kind: Some(Kind::StringValue((*v).to_string())),
            },
        );
    }
    DataMap { fields }
}

async fn create_user_and_token(
    client: &mut ContentApiClient<tonic::transport::Channel>,
    email: &str,
    password: &str,
) -> String {
    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", email),
                ("name", email),
                ("password", password),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");
    client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: email.to_string(),
            password: password.to_string(),
        })
        .await
        .expect("login")
        .into_inner()
        .token
}

fn with_bearer<T>(req: T, token: &str) -> Request<T> {
    let mut r = Request::new(req);
    let bearer: MetadataValue<_> = format!("Bearer {token}").parse().expect("valid metadata");
    r.metadata_mut().insert("authorization", bearer);
    r
}

// ── trigger_job_queues_run_visible_via_get_and_list ──────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn trigger_job_queues_run_visible_via_get_and_list() {
    let ctx =
        spawn_grpc_server_with_jobs(vec![make_users_def()], vec![], vec![make_test_job()]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let token = create_user_and_token(&mut client, "jobs1@x.com", "password-12345").await;

    // ListJobs (no auth requirement on the e2e_metadata_auth side, but
    // this handler does require it). Sanity that our job is visible.
    let jobs = client
        .list_jobs(with_bearer(ListJobsRequest {}, &token))
        .await
        .expect("list_jobs")
        .into_inner()
        .jobs;
    assert!(
        jobs.iter().any(|j| j.slug == "cleanup"),
        "registered job 'cleanup' should appear in ListJobs, got: {:?}",
        jobs.iter().map(|j| j.slug.as_str()).collect::<Vec<_>>()
    );

    // TriggerJob → returns the queued run's nanoid.
    let job_id = client
        .trigger_job(with_bearer(
            TriggerJobRequest {
                slug: "cleanup".to_string(),
                data: Some(r#"{"foo":"bar"}"#.to_string()),
                priority: None,
                delay: None,
                unique: None,
            },
            &token,
        ))
        .await
        .expect("trigger_job")
        .into_inner()
        .job_id;
    assert!(!job_id.is_empty(), "trigger should return a non-empty id");

    // GetJobRun by that id.
    let run = client
        .get_job_run(with_bearer(GetJobRunRequest { id: job_id.clone() }, &token))
        .await
        .expect("get_job_run")
        .into_inner()
        .run
        .expect("GetJobRunResponse should carry the run");
    assert_eq!(run.id, job_id);
    assert_eq!(run.slug, "cleanup");
    assert_eq!(
        run.status(),
        JobRunStatus::Pending,
        "scheduler isn't running in tests, run stays pending"
    );
    assert_eq!(run.data, r#"{"foo":"bar"}"#, "data should round-trip");
    // `attempt` reads as 0 for pending runs (scheduler bumps it to 1
    // on first execution); just sanity-check it's not garbage.
    assert!(run.attempt <= run.max_attempts);

    // ListJobRuns picks up the same run.
    let runs = client
        .list_job_runs(with_bearer(
            ListJobRunsRequest {
                slug: Some("cleanup".to_string()),
                ..Default::default()
            },
            &token,
        ))
        .await
        .expect("list_job_runs")
        .into_inner()
        .runs;
    assert!(
        runs.iter().any(|r| r.id == job_id),
        "ListJobRuns should include the newly triggered run"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── trigger_job_unknown_slug_returns_not_found ───────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn trigger_job_unknown_slug_returns_not_found() {
    let ctx =
        spawn_grpc_server_with_jobs(vec![make_users_def()], vec![], vec![make_test_job()]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let token = create_user_and_token(&mut client, "jobs2@x.com", "password-12345").await;

    let status = client
        .trigger_job(with_bearer(
            TriggerJobRequest {
                slug: "no-such-job".to_string(),
                data: None,
                priority: None,
                delay: None,
                unique: None,
            },
            &token,
        ))
        .await
        .expect_err("trigger of unknown slug should fail");
    assert_eq!(
        status.code(),
        Code::NotFound,
        "unknown job slug → NOT_FOUND, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── get_job_run_unknown_id_returns_not_found ─────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn get_job_run_unknown_id_returns_not_found() {
    let ctx =
        spawn_grpc_server_with_jobs(vec![make_users_def()], vec![], vec![make_test_job()]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let token = create_user_and_token(&mut client, "jobs3@x.com", "password-12345").await;

    let status = client
        .get_job_run(with_bearer(
            GetJobRunRequest {
                id: "no-such-run".to_string(),
            },
            &token,
        ))
        .await
        .expect_err("get_job_run for unknown id should fail");
    assert_eq!(
        status.code(),
        Code::NotFound,
        "unknown run id → NOT_FOUND, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
