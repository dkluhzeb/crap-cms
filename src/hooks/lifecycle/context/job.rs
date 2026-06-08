//! Job handler context — passed to the function configured on a
//! `crap.jobs.define` definition.

use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::typegen::lua::LuaAnnotation;

/// Job handler context. Passed to the handler function configured on a
/// `crap.jobs.define` definition.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.JobHandlerContext")]
pub struct JobHandlerContext<'a> {
    /// Input data from `crap.jobs.queue(...)` (or `{}` for cron-only
    /// runs).
    #[lua(ty = "table<string, any>")]
    pub data: &'a JsonValue,
    /// Job metadata for the current run.
    pub job: JobInfo<'a>,
}

/// Job metadata for the current run.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.JobInfo")]
pub struct JobInfo<'a> {
    /// Nanoid id of this job run (e.g. to record it or correlate logs).
    pub id: &'a str,
    /// Job definition slug.
    pub slug: &'a str,
    /// Queue this run is executing on.
    pub queue: &'a str,
    /// Current attempt number (1-based).
    pub attempt: u32,
    /// Total max attempts.
    pub max_attempts: u32,
    /// Scheduling priority this run was queued with (higher = claimed sooner).
    pub priority: i32,
    /// Dedup key, when the run was queued with `{ unique = "..." }`. `nil` for
    /// normal enqueues.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub unique_key: Option<&'a str>,
    /// How this run was triggered: `"cron"`, `"hook"`, `"grpc"`, or `"cli"`.
    /// `nil` if unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub scheduled_by: Option<&'a str>,
    /// ISO-8601 timestamp when the run was queued. `nil` if unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub queued_at: Option<&'a str>,
}
