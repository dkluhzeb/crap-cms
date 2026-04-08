//! Result type for write operations.

use std::collections::HashMap;

use serde_json::Value;

use crate::core::Document;

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, HashMap<String, Value>);
