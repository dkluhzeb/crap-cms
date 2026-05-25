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
    /// Job definition slug.
    pub slug: &'a str,
    /// Current attempt number (1-based).
    pub attempt: u32,
    /// Total max attempts.
    pub max_attempts: u32,
}
