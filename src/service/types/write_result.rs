//! Result type for write operations.

use crate::{
    core::{Document, ReqContext},
    service::EventRow,
};

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, ReqContext);

/// A write's result paired with the stored row its live event is built from
/// (see [`EventRow`]) — `None` when the write publishes no event. The row is
/// captured before the reported document is shaped and stripped for the
/// writer; only the event publisher consumes it, and it is never returned to a
/// caller outside the service layer.
pub(crate) type Gated<T> = (T, Option<EventRow>);
