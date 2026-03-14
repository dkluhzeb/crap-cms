//! Builder for [`PublishEventInput`].

use std::collections::HashMap;

use serde_json::Value as JsonValue;

use crate::core::{
    DocumentId, Slug,
    event::{EventOperation, EventTarget, EventUser},
};

use super::broadcast::PublishEventInput;

/// Builder for [`PublishEventInput`]. Created via [`PublishEventInput::builder`].
pub struct PublishEventInputBuilder {
    target: EventTarget,
    operation: EventOperation,
    collection: Option<Slug>,
    document_id: Option<DocumentId>,
    data: HashMap<String, JsonValue>,
    edited_by: Option<EventUser>,
}

impl PublishEventInputBuilder {
    pub(crate) fn new(target: EventTarget, operation: EventOperation) -> Self {
        Self {
            target,
            operation,
            collection: None,
            document_id: None,
            data: HashMap::new(),
            edited_by: None,
        }
    }

    pub fn collection(mut self, collection: impl Into<Slug>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    pub fn document_id(mut self, document_id: impl Into<DocumentId>) -> Self {
        self.document_id = Some(document_id.into());
        self
    }

    pub fn data(mut self, data: HashMap<String, JsonValue>) -> Self {
        self.data = data;
        self
    }

    pub fn edited_by(mut self, edited_by: Option<EventUser>) -> Self {
        self.edited_by = edited_by;
        self
    }

    pub fn build(self) -> PublishEventInput {
        PublishEventInput {
            target: self.target,
            operation: self.operation,
            collection: self.collection.expect("collection is required"),
            document_id: self.document_id.expect("document_id is required"),
            data: self.data,
            edited_by: self.edited_by,
        }
    }
}
