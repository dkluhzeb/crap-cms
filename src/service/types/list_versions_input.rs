//! Input for `list_versions` — version listing with pagination.

/// Input for [`list_versions`](crate::service::list_versions).
pub struct ListVersionsInput<'a> {
    pub parent_id: &'a str,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl<'a> ListVersionsInput<'a> {
    #[must_use]
    pub fn builder(parent_id: &'a str) -> ListVersionsInputBuilder<'a> {
        ListVersionsInputBuilder::new(parent_id)
    }
}

/// Builder for [`ListVersionsInput`].
pub struct ListVersionsInputBuilder<'a> {
    parent_id: &'a str,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl<'a> ListVersionsInputBuilder<'a> {
    pub fn new(parent_id: &'a str) -> Self {
        Self {
            parent_id,
            limit: None,
            offset: None,
        }
    }

    pub fn limit(mut self, limit: Option<i64>) -> Self {
        self.limit = limit;
        self
    }

    pub fn offset(mut self, offset: Option<i64>) -> Self {
        self.offset = offset;
        self
    }

    pub fn build(self) -> ListVersionsInput<'a> {
        ListVersionsInput {
            parent_id: self.parent_id,
            limit: self.limit,
            offset: self.offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_to_no_pagination() {
        let i = ListVersionsInput::builder("doc-1").build();
        assert_eq!(i.parent_id, "doc-1");
        assert!(i.limit.is_none());
        assert!(i.offset.is_none());
    }

    /// `limit` and `offset` share a type — distinct values catch a swap.
    #[test]
    fn builder_keeps_limit_and_offset_distinct() {
        let i = ListVersionsInput::builder("doc-1")
            .limit(Some(10))
            .offset(Some(20))
            .build();
        assert_eq!(i.limit, Some(10));
        assert_eq!(i.offset, Some(20));
    }
}
