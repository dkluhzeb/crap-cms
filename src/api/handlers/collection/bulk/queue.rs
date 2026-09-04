//! Shared queued-bulk codec path (`queue = true` on the bulk RPCs):
//! resolve the actor, stamp `queued_by`, insert the `_system_bulk` run.

use std::sync::Arc;

use tokio::task;
use tonic::Status;
use tracing::error;

use crate::{
    api::handlers::ContentService,
    service::{
        jobs::bulk_queue::{self, BulkJobData, QueuedBy},
        op::{self, Principal},
    },
};

/// Why a queue attempt failed, so each cause keeps its own gRPC status.
enum QueueError {
    Anonymous,
    Core(op::CoreError),
    /// A service-level refusal (access denied, over the document cap) that
    /// keeps the status the synchronous call would have returned.
    Service(crate::service::ServiceError),
    UnknownCollection(String),
    Insert(String),
}

impl ContentService {
    /// Resolve the caller and insert a `_system_bulk` job run carrying
    /// `data`; returns the job-run id for the response.
    ///
    /// Anonymous callers are rejected: a queued run executes later under
    /// the actor snapshotted here, so there must BE one.
    pub(in crate::api::handlers) async fn queue_bulk_run(
        &self,
        principal: Principal,
        mut data: BulkJobData,
    ) -> Result<String, Status> {
        let infra = Arc::clone(&self.infra);

        let queued = task::spawn_blocking(move || {
            // Auth failures keep their real status (the synchronous path's
            // mapping) instead of collapsing to INTERNAL.
            let (actor, override_access) =
                op::resolve_queue_actor(&infra, principal).map_err(QueueError::Core)?;

            data.queued_by = match (actor, override_access) {
                (_, true) => QueuedBy::System,
                (Some(actor), false) => {
                    data.ui_locale = actor.ui_locale;
                    QueuedBy::User {
                        id: actor.id,
                        collection: actor.collection,
                        session_version: actor.session_version,
                    }
                }
                (None, false) => return Err(QueueError::Anonymous),
            };

            // The document cap lives inside `queue_bulk` (one insert
            // chokepoint, every surface). Here we add the access gate,
            // which needs the resolved identity: a caller who may not
            // perform the operation is refused synchronously rather than
            // handed a job id for work that could never succeed.
            let conn = infra
                .pool
                .get()
                .map_err(|e| QueueError::Insert(format!("pool: {e}")))?;

            let def = infra
                .registry
                .get_collection(&data.collection)
                .ok_or_else(|| QueueError::UnknownCollection(data.collection.clone()))?;

            if matches!(data.queued_by, QueuedBy::User { .. }) {
                bulk_queue::check_queue_access(
                    &infra.hook_runner,
                    &conn,
                    &infra.registry,
                    def,
                    &data,
                )
                .map_err(QueueError::Service)?;
            }

            drop(conn);

            bulk_queue::queue_bulk(&infra.pool, &data)
                .map(|run| run.id)
                .map_err(|e| QueueError::Insert(format!("{e:?}")))
        })
        .await
        .map_err(|e| {
            error!("queue_bulk task error: {e}");
            Status::internal("Internal error")
        })?;

        queued.map_err(|e| match e {
            QueueError::Anonymous => {
                Status::unauthenticated("Queueing a bulk operation requires authentication")
            }
            QueueError::Core(core) => self.core_error_status(core),
            QueueError::Service(e) => {
                self.core_error_status(crate::service::op::CoreError::Service(e))
            }
            QueueError::UnknownCollection(slug) => {
                Status::not_found(format!("Collection '{slug}' not found"))
            }
            QueueError::Insert(detail) => {
                error!("queue_bulk insert: {detail}");
                Status::internal("Internal error")
            }
        })
    }
}
