//! Queued bulk operations (`queue = true` on the gRPC/MCP bulk ops).
//!
//! Instead of holding one write transaction for a whole synchronous batch,
//! the request is stored as a `_system_bulk` job run and executed later by
//! the scheduler (`scheduler::bulk`) — same atomic service op, same access
//! checks, run under the actor snapshotted at queue time. The caller gets
//! the job-run id and polls `GetJobRun` for status + the result summary.
//!
//! Visibility: `_system_bulk` runs have no registered `JobDefinition`, so
//! they never appear in `ListJobRuns` and `GetJobRun` resolves them ONLY
//! for the actor that queued them (the run's `data` carries the full
//! request payload) — see [`can_read_bulk_run`].

use std::borrow::Cow;

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::{from_str, to_string};

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, DocumentFields, Registry,
        job::{JobRun, SYSTEM_BULK_JOB, SYSTEM_BULK_QUEUE},
    },
    db::{AccessResult, DbConnection, DbPool, LocaleContext, query},
    hooks::AccessCheckInput,
    service::{AppInfra, ServiceError, collections::delete_scope},
};

/// Which bulk operation a `_system_bulk` run executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkOpKind {
    CreateMany,
    UpdateMany,
    DeleteMany,
}

/// The actor a bulk run executes as, stored as a REFERENCE and re-resolved
/// when the run executes: a lock, deletion, or session-version bump
/// (force-logout, password reset, unverify) between queueing and execution
/// abandons the run.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueuedBy {
    /// An authenticated user, stored as a REFERENCE (id, auth collection,
    /// session version) rather than as a document snapshot: the executor
    /// re-loads the user, so (a) no user fields — including ones hidden by
    /// field-level read access — are persisted in the jobs table or exposed
    /// through `GetJobRun.data`, and (b) a lock/deletion between queueing
    /// and execution is observed instead of ignored.
    User {
        id: String,
        collection: String,
        /// The queuer's `_session_version` at queue time. Re-checked at
        /// execution so a force-logout / password reset / unverify — which
        /// bump the version WITHOUT locking the account — also abandons a
        /// pending run, not just `lock_user`.
        #[serde(default)]
        session_version: u64,
    },
    /// An override caller (MCP): executes with `override_access`.
    System,
}

/// The identity-only projection of [`BulkJobData`] — the shape a
/// FINISHED run's stripped payload still carries, and all the
/// visibility rule ([`can_read_bulk_run`]) needs. `GetJobRun` decodes
/// this, never the full struct, so stripping can't hide a run from its
/// queuer.
#[derive(Serialize, Deserialize)]
pub struct BulkRunIdentity {
    pub queued_by: QueuedBy,
    /// The collection the run operates on. `None` for a run finished before
    /// the stripped payload kept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
}

/// The payload a finished run keeps in place of its stored data: its
/// [`BulkRunIdentity`] — all the visibility rules read — and nothing of the
/// request. Data that doesn't decode keeps nothing.
#[must_use]
pub fn finished_payload(data: &str) -> String {
    from_str::<BulkRunIdentity>(data)
        .ok()
        .and_then(|identity| to_string(&identity).ok())
        .unwrap_or_else(|| "{}".to_string())
}

/// The stored `_system_bulk` job payload — everything needed to rebuild
/// the operation at execution time.
#[derive(Serialize, Deserialize)]
pub struct BulkJobData {
    pub op: BulkOpKind,
    pub collection: String,
    pub queued_by: QueuedBy,
    #[serde(default)]
    pub locale: Option<String>,
    /// The queuing user's admin UI locale, so hook contexts match a
    /// synchronous call.
    #[serde(default)]
    pub ui_locale: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default = "default_true")]
    pub hooks: bool,
    #[serde(default)]
    pub events: bool,
    /// `[server] bulk_max_documents`, captured at queue time.
    pub max_documents: i64,
    /// `create_many`: the item payloads. Passwords are rejected at queue
    /// time (they would persist in the jobs table), so plain data only.
    #[serde(default)]
    pub documents: Option<Vec<DocumentFields>>,
    /// `update_many` / `delete_many`: the raw wire `where` JSON string —
    /// re-decoded at execution through the same `decode_where_json`
    /// chokepoint the synchronous codecs use.
    #[serde(default, rename = "where")]
    pub where_clause: Option<String>,
    /// `update_many`: the field values to set.
    #[serde(default)]
    pub data: Option<DocumentFields>,
    #[serde(default)]
    pub force_hard_delete: bool,
}

fn default_true() -> bool {
    true
}

/// Whether `user` (with `override_access`) may read a `_system_bulk` run.
/// Pure so the rule is unit-testable: override sees everything; a
/// user-queued run is visible to that user only; a system-queued run is
/// override-only.
#[must_use]
pub fn can_read_bulk_run(
    queued_by: &QueuedBy,
    user: Option<&Document>,
    override_access: bool,
) -> bool {
    if override_access {
        return true;
    }

    match queued_by {
        QueuedBy::System => false,
        // NOTE: compares the id only. The stored `collection` is not
        // compared because the reader's own issuing collection is not
        // threaded into `get_job_run` yet; ids are server-minted nanoids,
        // so a cross-collection collision is not attacker-constructible.
        QueuedBy::User { id, .. } => user.is_some_and(|u| u.id.as_ref() == id),
    }
}

/// Reject a queue attempt whose payload exceeds the configured
/// `server.bulk_max_documents`, before anything is persisted. The op
/// enforces the same cap on the match-set at execution; this closes the
/// window where an over-limit batch is stored only to fail later.
///
/// # Errors
///
/// [`ServiceError::LimitExceeded`] when the document count is over the cap.
pub fn enforce_queue_limit(data: &BulkJobData) -> Result<(), ServiceError> {
    let Some(documents) = data.documents.as_ref() else {
        return Ok(());
    };

    let count = i64::try_from(documents.len()).unwrap_or(i64::MAX);
    if data.max_documents > 0 && count > data.max_documents {
        return Err(ServiceError::LimitExceeded(format!(
            "create_many matched {count} documents, exceeding the configured limit of {}",
            data.max_documents
        )));
    }

    Ok(())
}

/// The definition a queued run's access gate must judge: the one the run
/// executes with. A forced hard delete clears soft delete first, exactly as
/// the operation does, so its gate is `access.delete` rather than the trash
/// gate — and a plain delete on a soft-delete collection is gated by trash.
fn gate_definition<'a>(
    def: &'a CollectionDefinition,
    data: &BulkJobData,
) -> Cow<'a, CollectionDefinition> {
    if data.op == BulkOpKind::DeleteMany && data.force_hard_delete && def.soft_delete {
        let mut hard = def.clone();
        hard.make_hard_delete();
        return Cow::Owned(hard);
    }

    Cow::Borrowed(def)
}

/// Load the user a run was queued by, for the queue-time gate — in the default
/// locale.
///
/// A DB error resolving the principal must PROPAGATE: swallowing it into
/// `None` would run the queue-time access gate with an anonymous principal on
/// a transient failure. (Execution re-gates fail-closed either way; this keeps
/// the early answer honest too.)
fn resolve_queuing_user(
    conn: &dyn DbConnection,
    registry: &Registry,
    locale_config: &LocaleConfig,
    queued_by: &QueuedBy,
) -> Result<Option<Document>, ServiceError> {
    let QueuedBy::User { id, collection, .. } = queued_by else {
        return Ok(None);
    };
    let Some(user_def) = registry.get_collection(collection) else {
        return Ok(None);
    };

    // In the default locale, as every internal read of a possibly-localized
    // row is: a localized auth collection has no bare columns to select.
    let locale_ctx = LocaleContext::default_for(locale_config);

    query::find_by_id(conn, collection, user_def, id, locale_ctx.as_ref())
        .map_err(|e| ServiceError::Internal(e.context("resolving queuing user")))
}

/// The collection-level access gate, run at QUEUE time so a caller who may
/// not perform the operation is refused synchronously instead of being
/// handed a `job_id` for work that can only fail. Execution still runs the
/// full per-document gate — this is an early, cheap rejection, never a
/// replacement.
///
/// # Errors
///
/// [`ServiceError::AccessDenied`] when the collection gate denies.
pub fn check_queue_access(
    infra: &AppInfra,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    data: &BulkJobData,
) -> Result<(), ServiceError> {
    // Resolve the queuing user HERE rather than in the codec: surfaces call
    // service functions, never the query layer directly (the surface-parity
    // test enforces exactly this).
    let user = resolve_queuing_user(conn, &infra.registry, &infra.locale_config, &data.queued_by)?;

    let gate_def = gate_definition(def, data);
    let (operation, access_fn) = match data.op {
        BulkOpKind::CreateMany => ("create", gate_def.access.create.as_ref()),
        BulkOpKind::UpdateMany => ("update", gate_def.access.update.as_ref()),
        // The derivation the operation's own bulk gate uses: the trash gate
        // when soft-deleting, `access.delete` when deleting permanently.
        BulkOpKind::DeleteMany => {
            let scope = delete_scope(&gate_def);
            (scope.operation, scope.access_fn)
        }
    };

    let result = infra
        .hook_runner
        .check_access(
            &AccessCheckInput::builder(operation, &data.collection)
                .access(access_fn)
                .user(user.as_ref())
                .build(),
            conn,
        )
        .map_err(ServiceError::Internal)?;

    match result {
        // A row-filter result is not a denial: the per-document gate at
        // execution applies it. Only an outright denial is refused here.
        AccessResult::Allowed | AccessResult::Constrained(_) => Ok(()),
        AccessResult::Denied => Err(ServiceError::AccessDenied(format!(
            "{operation} access denied for collection '{}'",
            data.collection
        ))),
    }
}

/// Insert a `_system_bulk` job run with exactly one attempt (see below).
///
/// # Errors
///
/// Returns a backend error when serialization, the pool, or the INSERT fails.
pub fn queue_bulk(pool: &DbPool, data: &BulkJobData) -> Result<JobRun, ServiceError> {
    // Enforced HERE, not per surface: this is the one insert site, so no
    // codec can forget the cap (gRPC used to check it and MCP did not).
    enforce_queue_limit(data)?;

    // ALWAYS a single attempt, regardless of `[jobs.queues.bulk] retries`.
    // A batch is atomic, but a crash (or an outer-timer requeue) in the
    // window between its commit and the completion mark would make a retry
    // re-apply the whole batch. Enforced here at the one insert chokepoint
    // rather than relying on a config default an operator can change.
    let max_attempts = 1;

    let json =
        to_string(data).map_err(|e| ServiceError::Internal(anyhow!("bulk job serialize: {e}")))?;

    // The write pool: this INSERT competes with the batch writes themselves.
    let conn = pool.write().map_err(ServiceError::Internal)?;

    query::jobs::insert_job(
        &conn,
        SYSTEM_BULK_JOB,
        &json,
        "api",
        max_attempts,
        SYSTEM_BULK_QUEUE,
        0,
    )
    .map_err(ServiceError::Internal)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::{CrapConfig, DatabaseConfig},
        core::{FieldDefinition, FieldType, collection::Auth},
        db::{migrate, pool},
    };

    /// Regression: the queue-time gate loaded the queuing user without a
    /// locale, and a read of an auth collection with a localized field selects
    /// columns that don't exist — so queueing failed for its users.
    #[test]
    fn the_queuing_user_is_read_in_the_default_locale() {
        let tmp = tempfile::tempdir().unwrap();
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        let locale = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: false,
        };
        let mut users = CollectionDefinition::new("users");
        users.auth = Some(Auth::new(true));
        users.fields = vec![
            FieldDefinition::builder("bio", FieldType::Text)
                .localized(true)
                .build(),
        ];
        let shared = Registry::shared();
        shared.write().unwrap().register_collection(users);
        let registry = Registry::snapshot(&shared);
        migrate::sync_all(&db_pool, &registry, &locale).unwrap();
        let conn = db_pool.get().unwrap();
        conn.execute("INSERT INTO users (id) VALUES ('u1')", &[])
            .unwrap();

        let queued_by = QueuedBy::User {
            id: "u1".to_string(),
            collection: "users".to_string(),
            session_version: 0,
        };
        let user = resolve_queuing_user(&conn, &registry, &locale, &queued_by).unwrap();

        assert_eq!(user.map(|u| u.id.to_string()), Some("u1".to_string()));
    }

    /// The internal bulk path inserts the `_system_bulk` slug directly — it
    /// must keep working while the caller-facing trigger chokepoint refuses
    /// every system slug.
    #[test]
    fn queue_bulk_still_inserts_the_system_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        let registry = Registry::new();
        migrate::sync_all(&db_pool, &registry, &config.locale).unwrap();

        let run = queue_bulk(&db_pool, &delete_job(false)).expect("system path queues");

        assert_eq!(run.slug, SYSTEM_BULK_JOB);
        assert_eq!(run.max_attempts, 1, "a bulk run is always a single attempt");
    }

    fn delete_job(force_hard_delete: bool) -> BulkJobData {
        BulkJobData {
            op: BulkOpKind::DeleteMany,
            collection: "posts".into(),
            queued_by: QueuedBy::System,
            locale: None,
            ui_locale: None,
            draft: false,
            hooks: true,
            events: false,
            max_documents: 0,
            documents: None,
            where_clause: None,
            data: None,
            force_hard_delete,
        }
    }

    /// The queue-time gate derives from the definition the run executes with:
    /// trash on a soft-delete collection, `access.delete` once a hard delete is
    /// forced. It used to check `access.delete` for both.
    #[test]
    fn queued_delete_gate_matches_the_executed_operation() {
        let mut def = CollectionDefinition::new("posts");
        def.soft_delete = true;
        def.access.delete = Some("can_delete".into());
        def.access.trash = Some("can_trash".into());

        let soft = gate_definition(&def, &delete_job(false));
        assert_eq!(delete_scope(&soft).operation, "trash");

        let hard = gate_definition(&def, &delete_job(true));
        assert_eq!(delete_scope(&hard).operation, "delete");
    }

    #[test]
    fn job_data_round_trips_with_where_rename() {
        let data = BulkJobData {
            op: BulkOpKind::UpdateMany,
            collection: "posts".into(),
            queued_by: QueuedBy::User {
                id: "u1".to_string(),
                collection: "users".to_string(),
                session_version: 0,
            },
            locale: None,
            ui_locale: None,
            draft: false,
            hooks: true,
            events: false,
            max_documents: 100,
            documents: None,
            where_clause: Some(json!({"status": {"equals": "draft"}}).to_string()),
            data: Some(
                [("status".to_string(), json!("published"))]
                    .into_iter()
                    .collect(),
            ),
            force_hard_delete: false,
        };

        let s = to_string(&data).unwrap();
        assert!(s.contains("\"where\""), "wire spelling: {s}");

        let back: BulkJobData = from_str(&s).unwrap();
        assert_eq!(back.op, BulkOpKind::UpdateMany);
        assert!(matches!(back.queued_by, QueuedBy::User { .. }));
        assert!(back.where_clause.is_some());
    }

    /// The visibility rule: queuer-only, override sees all, system-queued
    /// runs are override-only.
    #[test]
    fn bulk_run_visibility_matrix() {
        let mine = QueuedBy::User {
            id: "u1".to_string(),
            collection: "users".to_string(),
            session_version: 0,
        };
        let theirs = QueuedBy::User {
            id: "u2".to_string(),
            collection: "users".to_string(),
            session_version: 0,
        };
        let system = QueuedBy::System;

        let me = Document::new("u1");

        assert!(can_read_bulk_run(&mine, Some(&me), false));
        assert!(!can_read_bulk_run(&theirs, Some(&me), false));
        assert!(!can_read_bulk_run(&system, Some(&me), false));
        assert!(!can_read_bulk_run(&mine, None, false));

        for q in [&mine, &theirs, &system] {
            assert!(can_read_bulk_run(q, None, true), "override reads all");
        }
    }
}
