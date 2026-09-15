//! Input for `list_versions` — version listing with pagination.

use crate::{core::Builder, db::LocaleContext};

/// Input for [`list_versions`](crate::service::list_versions).
#[derive(Builder)]
pub struct ListVersionsInput<'a> {
    #[builder(required)]
    pub parent_id: &'a str,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// The locale each snapshot is returned in. Absent = the default locale,
    /// matching an unqualified read of the document.
    pub locale_ctx: Option<&'a LocaleContext>,
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
        assert!(i.locale_ctx.is_none());
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
