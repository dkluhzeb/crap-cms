//! Result type for write operations.

use crate::core::{Document, ReqContext};

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, ReqContext);
