//! Statement budgets on Postgres (see [`crate::db::deadline`]): a statement
//! still running when its budget runs out is cancelled on the server.

use std::{future::Future, time::Duration};

use anyhow::{Error, Result};
use tokio::{pin, time::timeout};
use tokio_postgres::{CancelToken, NoTls};
use tracing::warn;

use super::stmt_cache::PgResult;
use crate::db::deadline::StatementTimedOut;

/// Run one statement's `work` within `budget` (`None`: unbounded). Out of
/// time, the server is asked to cancel the statement through `cancel`, and
/// its own outcome — the cancellation, unless it finished just then — is
/// awaited, so the connection is never left mid-statement.
///
/// # Errors
///
/// The statement's error; a [`StatementTimedOut`] in its chain when the
/// budget ran out.
pub(super) async fn bounded<T>(
    budget: Option<Duration>,
    cancel: CancelToken,
    work: impl Future<Output = PgResult<T>>,
) -> Result<T> {
    let Some(budget) = budget else {
        return work.await.map_err(Error::new);
    };

    pin!(work);

    if let Ok(result) = timeout(budget, &mut work).await {
        return result.map_err(Error::new);
    }

    if let Err(e) = cancel.cancel_query(NoTls).await {
        warn!("Could not cancel a Postgres statement past its time limit: {e}");
    }

    work.await
        .map_err(|e| Error::new(e).context(StatementTimedOut))
}
